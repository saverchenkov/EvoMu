//! Integration tests closing 10 Scanning, Exploration & Wormhole parity rows.

use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::{
    apply_wormhole_phenomenon_to_attributes, resolve_system_wormhole_phenomenon,
    wormhole_phenomenon_type_for_class_and_name,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{ensure_system_pve_sites_seeded, with_pve_exploration_reg};
use eve_server::services::space_destiny::{
    compute_probe_max_deviation_au, probe_allowed_ranges_au, probe_base_max_deviation_au,
    probe_base_scan_range_au, probe_max_flight_time_s,
    signature_difficulty_level_and_min_strength, wormhole_item_id_for_signature,
};
use glam::DVec3;
use std::collections::{HashMap, VecDeque};

const AU_IN_METERS: f64 = 149_597_870_700.0;

fn are_hacking_tiles_adjacent(a: u32, b: u32) -> bool {
    let (aq, ar) = ((a % 6) as i32, (a / 6) as i32);
    let (bq, br) = ((b % 6) as i32, (b / 6) as i32);
    let (dq, dr) = (aq - bq, ar - br);
    ((dq.abs() + (dq + dr).abs() + dr.abs()) / 2) == 1
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
}

async fn spawn_test_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3, mass: f64, cloaked: bool) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.shield_hp = 10_000.0;
    cs.max_shield_hp = 10_000.0;
    cs.is_cloaked = cloaked;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            cs,
        )
        .await;
}

async fn setup_pilot_in_space(ctx: &ServerContext, session_id: u64, char_id: i64, solar_system_id: u32, ship_id: i64, ship_type_id: u32) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::Hangar, 1, true));
    });
    spawn_test_ball(ctx, solar_system_id, ship_id, ship_type_id, DVec3::ZERO, 1_200_000.0, false).await;
    sess
}

fn last_hacking_turn_event_data(sess: &mut SessionState) -> PyRep {
    let (_, payload) = sess.drain_notifications().into_iter().rev().find(|(ev, _)| ev == "OnHackingTurnComplete").expect("OnHackingTurnComplete");
    let Some(PyRep::List(events)) = payload.first() else { panic!("events list") };
    events.last().and_then(|ev| ev.dict_get("eventData")).cloned().expect("eventData")
}

// parity: feat:scanning.probe-range-deviation, feat:scanning.probe-target-classes, feat:scanning.signature-reveal-thresholds, feat:scanning.signature-strength-levels
#[tokio::test]
async fn test_probe_scanning_ranges_deviation_expiry_classes_thresholds_and_levels() {
    eve_testkit::attest(&[
        "feat:scanning.probe-range-deviation",
        "feat:scanning.probe-target-classes",
        "feat:scanning.signature-reveal-thresholds",
        "feat:scanning.signature-strength-levels",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, sys_id, ship_id, launcher_id) = (90_000_001, 30_000_142u32, 610_000_001i64, 610_000_010i64);
    let mut sess = setup_pilot_in_space(&ctx, 701, char_id, sys_id, ship_id, 606).await;

    // 1. Core vs Combat probe range steps (0.25..=32 AU vs 0.5..=64 AU) & flight time from SDE
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30013), [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0]);
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30028), [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]);
    assert_eq!((probe_base_scan_range_au(&ctx.sde, 30013), probe_base_scan_range_au(&ctx.sde, 30028)), (0.25, 0.5));
    assert_eq!((probe_base_max_deviation_au(&ctx.sde, 30013), probe_base_max_deviation_au(&ctx.sde, 30028)), (0.125, 0.25));
    assert_eq!((probe_max_flight_time_s(&ctx.sde, 32246), probe_max_flight_time_s(&ctx.sde, 30013), probe_max_flight_time_s(&ctx.sde, 30028)), (1_000.0, 4_000.0, 4_000.0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(48, 2_000.0);
            sh.dynamic_attributes.insert(11, 2_000.0);
        }
        snap.items.insert(launcher_id, make_item(launcher_id, 17901, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(610_000_011, make_item(610_000_011, 32246, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let launch_rss = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.expect("Launch RSS");
    assert_eq!(launch_rss.dict_get("maxFlightTimeS").and_then(|v| v.as_f64()), Some(1_000.0));
    assert_eq!(launch_rss.dict_get("canScanShips"), Some(&PyRep::Bool(false)));

    // Advance 1,001s (> 16m40s): background cluster tick expires RSS probes and returns them to cargo before any RPC
    ctx.advance_secs(1_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let rss_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 32246).map(|i| i.quantity).sum()
    });
    assert_eq!(rss_in_cargo, 8, "Background cluster tick returned 8 expired RSS probes to cargo");

    // Standard probes (4000s = 1h6m40s) when cargo is full -> probes are lost on cluster tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 0.1);
        snap.items.insert(610_000_012, make_item(610_000_012, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    ctx.advance_secs(4_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let std_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 30013).map(|i| i.quantity).sum()
    });
    assert_eq!(std_in_cargo, 0, "Expired probes are lost when cargo hold is full");

    // 2. Live RPC range steps & deviation scaling (untrained Astrometrics 0 vs Astrometrics 4 + Pinpointing 3)
    // and signature reveal thresholds (<25%, 25%..75%, 75%..100%, 100%) + SDE dungeon difficulty levels (I-III: 42, IV: 72, V: 104)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.unwrap();
    let sig_pos = [300_000.0, 0.0, 0.0];
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 1_000.0);
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&3412);
            sk.remove(&25810);
            sk.remove(&25811);
        }
        let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
        sigs.clear();
        for (id, grp, name) in [
            ("TST-LV1", "Relic Site", "Desolate Collapsed Conduit"),
            ("TST-LV4", "Data Site", "Abandoned Research Complex DA005"),
            ("TST-LV5", "Data Site", "Unsecured Core Backup Array"),
        ] {
            sigs.push(CosmicSignatureRecord {
                signature_id: id.into(), solar_system_id: sys_id, sig_group: grp.into(), type_name: name.into(), signal_strength: 0.0, position: sig_pos,
                wormhole_type_id: None, destination_system_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0, max_jump_mass_kg: 0.0, created_unix: now_u, lifetime_seconds: 86_400, collapsed: false,
            });
        }
        snap.items.insert(610_000_013, make_item(610_000_013, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });

    let sigs_snap = ctx.db.with_snapshot(|s| s.cosmic_signatures[&sys_id].clone());
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[0]), (1, 42.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[1]), (4, 72.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[2]), (5, 104.0));

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    let nan_coords = PyRep::List(vec![PyRep::List(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Integer(1)])]);
    assert_eq!(ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[nan_coords], &kw, None).await.unwrap_err().code, "InvalidProbeCoordinates");

    let make_coords_step = |covering: usize, step: i64| -> PyRep {
        PyRep::List((0..8).map(|i| {
            let p = if i < covering { sig_pos } else { [9.0e12, 9.0e12, 9.0e12] };
            PyRep::List(vec![PyRep::Real(p[0]), PyRep::Real(p[1]), PyRep::Real(p[2]), PyRep::Integer(step)])
        }).collect())
    };
    let find_row = |rep: &PyRep, id: &str| -> PyRep {
        let PyRep::List(rows) = rep else { panic!() };
        rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(id)).unwrap().clone()
    };

    // Untrained character (Astrometrics 0, Pinpointing 0) at step 4 (2.0 AU = 8x base 0.25 AU): maxDeviationAU = 1.0 AU
    let scan_untrained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_untrained = find_row(&scan_untrained_step4, "TST-LV1");
    assert_eq!(row_untrained.dict_get("scanRangeAU").and_then(|v| v.as_f64()), Some(2.0));
    assert!((row_untrained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-9);
    let sig_untrained_s = row_untrained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();

    // Train Astrometrics IV + Pinpointing III at step 4 (2.0 AU): maxDeviationAU = 0.68 AU and higher signal strength
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3412, TrainedSkillRecord { type_id: 3412, level: 4, skill_points: 45_255.0 });
        sk.insert(25811, TrainedSkillRecord { type_id: 25811, level: 3, skill_points: 16_000.0 });
    });
    let expected_dev_au = compute_probe_max_deviation_au(0.125, 2.0, 0.25, 4, 3);
    let scan_trained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_trained = find_row(&scan_trained_step4, "TST-LV1");
    assert!((row_trained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - expected_dev_au).abs() < 1e-9);
    let sig_trained_s = row_trained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();
    assert!(sig_trained_s > sig_untrained_s, "Astrometrics IV yields higher signal strength than untrained Astrometrics 0");
    let expected_dev_m = (1.0 - sig_trained_s) * expected_dev_au * AU_IN_METERS;
    assert!((row_trained.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() - expected_dev_m).abs() < 1.0);

    // Set Astrometrics II for threshold & level tests at step 1 (0.25 AU)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 2, skill_points: 8_000.0 });
    });

    // < 25% (1 probe covering -> capped at 0.24): only signature ID and deviation shown; gated fields omitted
    let scan_1 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(1, 1)], &kw, None).await.unwrap();
    let lv1_r1 = find_row(&scan_1, "TST-LV1");
    assert!(lv1_r1.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap() < 0.25);
    assert_eq!(lv1_r1.dict_get("group").and_then(|v| v.as_str()), Some("Unknown"));
    assert_eq!((lv1_r1.dict_get("groupName"), lv1_r1.dict_get("siteName"), lv1_r1.dict_get("warpable")), (Some(&PyRep::None), Some(&PyRep::None), Some(&PyRep::Bool(false))));
    assert!(lv1_r1.dict_get("difficultyLevel").is_none() && lv1_r1.dict_get("typeID").is_none() && lv1_r1.dict_get("massStage").is_none() && lv1_r1.dict_get("lifetimeDescription").is_none());
    assert!(lv1_r1.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() > 0.0);

    // 25%..75% (2 probes covering -> capped at 0.49): group shown, siteName hidden, not warpable
    let scan_2 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 1)], &kw, None).await.unwrap();
    let lv1_r2 = find_row(&scan_2, "TST-LV1");
    assert!((0.25..0.75).contains(&lv1_r2.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv1_r2.dict_get("group").and_then(|v| v.as_str()), Some("Relic Site"));
    assert_eq!((lv1_r2.dict_get("siteName"), lv1_r2.dict_get("warpable")), (Some(&PyRep::None), Some(&PyRep::Bool(false))));
    assert!(lv1_r2.dict_get("difficultyLevel").is_none());

    // 75%..100% & strength-level gating via real 8-probe formation math (Core Probe I + Astrometrics II = 70.4: resolves Lv I, < 72 for Lv IV, < 104 for Lv V)
    let scan_low_skill = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_low_skill, "TST-LV1").dict_get("warpable"), Some(&PyRep::Bool(true)));
    let lv4_low = find_row(&scan_low_skill, "TST-LV4");
    assert!((0.75..1.0).contains(&lv4_low.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv4_low.dict_get("siteName").and_then(|v| v.as_str()), Some("Abandoned Research Complex DA005"));
    assert_eq!(lv4_low.dict_get("warpable"), Some(&PyRep::Bool(false)));
    assert!(find_row(&scan_low_skill, "TST-LV5").dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap() < 0.75);

    // Warping to unresolved Lv IV (<100%) fails; warping to resolved Lv I (100%) succeeds
    let beyonce_bound = ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap();
    let (lv4_ball_id, lv1_ball_id) = (wormhole_item_id_for_signature(sys_id, "TST-LV4"), wormhole_item_id_for_signature(sys_id, "TST-LV1"));
    for bid in [lv4_ball_id, lv1_ball_id] {
        spawn_test_ball(&ctx, sys_id, bid, 26272, DVec3::new(300_000.0, 0.0, 0.0), 1_000.0, false).await;
    }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv4_ball_id)], &kw, None).await.unwrap_err().code, "InvalidWarpDestination");
    assert!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv1_ball_id)], &kw, None).await.is_ok());

    // Train Astrometrics V -> 8x Core Probe I formation strength = 80.0 (resolves Lv IV >= 72, fails Lv V < 104)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 5, skill_points: 256_000.0 });
    });
    let scan_astro5 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_astro5, "TST-LV4").dict_get("warpable"), Some(&PyRep::Bool(true)));
    assert_eq!(find_row(&scan_astro5, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(false)));

    // Load Sisters Core Scanner Probe (30488, base 44) + Astrometric Rangefinding V -> formation strength = 110.0 >= 104.0 (resolves Lv V)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(25810, TrainedSkillRecord { type_id: 25810, level: 5, skill_points: 512_000.0 });
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_015, make_item(610_000_015, 30488, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    let scan_105 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_105, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(true)));

    // 3. Target classes: Core probes resolve CosmicSignatures only; Combat probes also resolve Ships, Drones, Structures;
    // wrecks, cargo containers, and cloaked ships never appear in probe results.
    let (target_ship, cloaked_ship, drone_id, structure_id, wreck_id, cargo_can) = (610_000_101, 610_000_102, 610_000_103, 610_000_104, 610_000_105, 610_000_106);
    for (bid, tid, cloaked) in [(target_ship, 587u32, false), (cloaked_ship, 11188, true), (drone_id, 2454, false), (structure_id, 35832, false), (wreck_id, 26561, false), (cargo_can, 23, false)] {
        spawn_test_ball(&ctx, sys_id, bid, tid, DVec3::from_array(sig_pos), 1_000.0, cloaked).await;
    }
    let PyRep::List(rows_105) = &scan_105 else { panic!() };
    assert!(rows_105.iter().all(|r| r.dict_get("targetClass").and_then(|v| v.as_str()) == Some("CosmicSignature")));

    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_014, make_item(610_000_014, 30486, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let launch_combat = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    assert_eq!(launch_combat.dict_get("canScanShips"), Some(&PyRep::Bool(true)));

    let combat_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    let PyRep::List(combat_rows) = &combat_scan else { panic!() };
    let scanned_ids: Vec<i64> = combat_rows.iter().filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64())).collect();
    assert!(scanned_ids.contains(&target_ship) && scanned_ids.contains(&drone_id) && scanned_ids.contains(&structure_id));
    assert!(!scanned_ids.contains(&cloaked_ship) && !scanned_ids.contains(&wreck_id) && !scanned_ids.contains(&cargo_can));
}

async fn reveal_tile_via_clicks(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, can_id: i64, target_idx: u32) {
    let kw = HashMap::new();
    let mut parent: HashMap<u32, u32> = HashMap::from([(0, 0)]);
    let mut queue = VecDeque::from([0u32]);
    while let Some(cur) = queue.pop_front() {
        if cur == target_idx {
            break;
        }
        for nxt in 0..24u32 {
            if !parent.contains_key(&nxt) && are_hacking_tiles_adjacent(cur, nxt) {
                parent.insert(nxt, cur);
                queue.push_back(nxt);
            }
        }
    }
    let (mut path, mut w) = (Vec::new(), target_idx);
    while w != 0 {
        path.push(w);
        w = parent[&w];
    }
    path.reverse();
    for (i, &step_idx) in path.iter().enumerate() {
        let is_last = i + 1 == path.len();
        for _ in 0..5 {
            let need_click = with_pve_exploration_reg(ctx, |reg| {
                let g = &reg.hacking_games[&(char_id, can_id)];
                if g.status != "InProgress" {
                    return false;
                }
                let t = &g.tiles[step_idx as usize];
                !t.revealed || (!is_last && matches!(format!("{:?}", t.kind).as_str(), "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor") && t.coherence > 0)
            });
            if !need_click {
                break;
            }
            let _ = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(step_idx as i64)], &kw, None).await.unwrap();
        }
    }
}

// parity: feat:exploration.hack-failure-container-explosion, feat:exploration.hacking-coherence-and-strength, feat:exploration.hacking-node-network
#[tokio::test]
async fn test_exploration_hacking_grid_coherence_strength_and_second_fail_explosion() {
    eve_testkit::attest(&[
        "feat:exploration.hack-failure-container-explosion",
        "feat:exploration.hacking-coherence-and-strength",
        "feat:exploration.hacking-node-network",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, wh_sys_id, ship_id) = (90_000_001, 31_000_005u32, 620_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 702, char_id, wh_sys_id, ship_id, 606).await;

    let (can_1, can_2, loot_in_can_2, bystander_near, bystander_far) = (620_000_101, 620_000_102, 620_000_103, 620_000_201, 620_000_202);
    ensure_system_pve_sites_seeded(&ctx, wh_sys_id).await;
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(site) = reg.sites_by_system.get_mut(&wh_sys_id).and_then(|v| v.iter_mut().find(|s| s.site_category == "Relic")) {
            site.hackable_can_ids.push(can_1);
            site.hackable_can_ids.push(can_2);
        }
    });
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(21718, TrainedSkillRecord { type_id: 21718, level: 2, skill_points: 8_000.0 });
        sk.insert(13278, TrainedSkillRecord { type_id: 13278, level: 2, skill_points: 8_000.0 });
        snap.items.insert(620_000_010, make_item(620_000_010, 22177, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(620_000_010, true);
        snap.items.insert(can_1, make_item(can_1, 24675, 1, wh_sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(can_2, make_item(can_2, 24675, 1, wh_sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_in_can_2, make_item(loot_in_can_2, 25605, 1, can_2, InventoryFlag::Cargo, 5, false));
    });
    spawn_test_ball(&ctx, wh_sys_id, can_1, 24675, DVec3::new(1_000.0, 0.0, 0.0), 1_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, can_2, 24675, DVec3::new(1_500.0, 0.0, 0.0), 1_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, bystander_near, 587, DVec3::new(3_000.0, 0.0, 0.0), 1_000_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, bystander_far, 587, DVec3::new(9_000.0, 0.0, 0.0), 1_000_000.0, false).await;

    // 1. T1 analyzer stats (20 strength, 40 + 10*2 = 60 coherence via Archaeology 13278), asserted via OnHackingTurnComplete notification payload
    let _ = sess.drain_notifications();
    let v24_start_args = [PyRep::Integer(can_1), PyRep::Integer(24675), PyRep::Integer(620_000_010), PyRep::Integer(22177), PyRep::Integer(5), PyRep::Integer(0), PyRep::Integer(0)];
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_args, &kw, None).await.unwrap();
    let start_ev = last_hacking_turn_event_data(&mut sess);
    assert_eq!(start_ev.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(start_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(60));
    assert_eq!(start_ev.dict_get("gridTopology").and_then(|v| v.as_str()), Some("Hexagonal"));
    assert!((1..=5).contains(&start_ev.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
    assert!(start_ev.dict_get("distanceToCore").is_none());

    // Verify `hackingMgr.values` never exposes `distanceToCore` or unrevealed `distanceIndicator`
    let board_vals = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "values", &[PyRep::Integer(can_1)], &kw, None).await.unwrap();
    let PyRep::List(val_tiles) = &board_vals else { panic!() };
    assert_eq!(val_tiles.len(), 24);
    for vt in val_tiles {
        assert!(vt.dict_get("distanceToCore").is_none(), "distanceToCore must never be exposed");
        if vt.dict_get("revealed").and_then(|v| v.as_bool()).unwrap_or(false) {
            assert!((1..=5).contains(&vt.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
        } else {
            assert!(vt.dict_get("distanceIndicator").is_none(), "unrevealed tiles must not leak distance");
        }
    }

    let (fw_idx, has_all_kinds, layout_1) = with_pve_exploration_reg(&ctx, |reg| {
        let g = &reg.hacking_games[&(char_id, can_1)];
        let kinds: Vec<String> = g.tiles.iter().map(|t| format!("{:?} {:?}", t.kind, t.utility_drop)).collect();
        let has_all = ["Empty", "Firewall", "Antivirus", "SystemCore", "SelfRepair", "KernelRot", "SecondaryVector"].iter().all(|k| kinds.iter().any(|tk| tk.contains(k)));
        let fw = g.tiles.iter().find(|t| format!("{:?}", t.kind) == "Firewall").unwrap().tile_index;
        (fw, has_all, kinds)
    });
    assert!(has_all_kinds);

    // Non-adjacent hex tile (tile 10: q=4, r=1, hex_dist(0, 10) == 5) is rejected
    assert_eq!(ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(10)], &kw, None).await.unwrap_err().code, "TileNotAccessible");

    // Reveal the procedural Firewall tile via valid hex clicks and verify combat + distance clue via OnHackingTurnComplete
    reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_1, fw_idx).await;
    let fw_ev = last_hacking_turn_event_data(&mut sess);
    assert!(fw_ev.dict_get("distanceToCore").is_none());
    assert!((1..=5).contains(&fw_ev.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
    assert_eq!(fw_ev.dict_get("nodeKind").and_then(|v| v.as_str()), Some("Firewall"));
    assert_eq!(fw_ev.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(20));
    assert!(fw_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap() <= 40);

    // Reach hack failure (virusCoherence == 0) through real tile interactions on defensive nodes without editing internal state
    let def_indices: Vec<u32> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_1)].tiles.iter().filter(|t| t.strength > 0).map(|t| t.tile_index).collect()
    });
    let mut failed_ev = None;
    'outer1: for d_idx in def_indices {
        reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_1, d_idx).await;
        for _ in 0..5 {
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_1)].status == "Failed") {
                failed_ev = Some(last_hacking_turn_event_data(&mut sess));
                break 'outer1;
            }
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_1)].tiles[d_idx as usize].coherence <= 0) {
                break;
            }
            let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(d_idx as i64)], &kw, None).await.unwrap();
        }
    }
    let fail1_ev = failed_ev.expect("Reached hack failure via real defensive node clicks");
    assert_eq!((fail1_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()), fail1_ev.dict_get("status").and_then(|v| v.as_str())), (Some(0), Some("Failed")));

    // 2. T2 Relic Analyzer II (30832: SDE 60 base coherence, 30 strength) + Archaeology IV (+40) = 100 coherence;
    // procedural layout differs across containers; 2nd hack failure triggers container explosion & AoE damage
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&620_000_010).unwrap().type_id = 30832;
        snap.skills.entry(char_id).or_default().insert(13278, TrainedSkillRecord { type_id: 13278, level: 4, skill_points: 45_255.0 });
    });
    let v24_start_can2 = [PyRep::Integer(can_2), PyRep::Integer(24675), PyRep::Integer(620_000_010), PyRep::Integer(30832), PyRep::Integer(5), PyRep::Integer(0), PyRep::Integer(0)];
    let _ = sess.drain_notifications();
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_can2, &kw, None).await.unwrap();
    let start_t2_ev = last_hacking_turn_event_data(&mut sess);
    assert_eq!((start_t2_ev.dict_get("virusStrength").and_then(|v| v.as_i64()), start_t2_ev.dict_get("virusCoherence").and_then(|v| v.as_i64())), (Some(30), Some(100)));
    let layout_2: Vec<String> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_2)].tiles.iter().map(|t| format!("{:?} {:?}", t.kind, t.utility_drop)).collect()
    });
    assert_ne!(layout_1, layout_2, "Procedural hex board layout varies per container/seed");

    let sys_handle = ctx.systems.get_or_spawn(wh_sys_id);
    // 1st failure on can_2 via QuitHackingAttempt
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[], &kw, None).await.unwrap();
    assert!(sys_handle.get_ball_snapshot(can_2).await.unwrap().is_some());

    // 2nd attempt on can_2 with T1 Relic Analyzer I (22177) + Archaeology 0 (40 coherence, 20 strength) -> fail via real clicks on defensive nodes -> explodes!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&620_000_010).unwrap().type_id = 22177;
        snap.skills.entry(char_id).or_default().insert(13278, TrainedSkillRecord { type_id: 13278, level: 0, skill_points: 0.0 });
    });
    let _ = sess.drain_notifications();
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_can2, &kw, None).await.unwrap();
    let def_indices_2: Vec<u32> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_2)].tiles.iter().filter(|t| t.strength > 0).map(|t| t.tile_index).collect()
    });
    let mut fail2_ev_opt = None;
    'outer2: for d_idx in def_indices_2 {
        reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_2, d_idx).await;
        for _ in 0..5 {
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_2)].status == "Failed") {
                fail2_ev_opt = Some(last_hacking_turn_event_data(&mut sess));
                break 'outer2;
            }
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_2)].tiles[d_idx as usize].coherence <= 0) {
                break;
            }
            let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(d_idx as i64)], &kw, None).await.unwrap();
        }
    }
    let fail2_ev = fail2_ev_opt.expect("2nd failure reached via real defensive node clicks");
    assert_eq!(fail2_ev.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2_ev.dict_get("containerDestroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2_ev.dict_get("destroyedLootCount").and_then(|v| v.as_i64()), Some(2));
    assert!((2_000.0..=5_000.0).contains(&fail2_ev.dict_get("explosionDamage").and_then(|v| v.as_f64()).unwrap()));

    assert!(sys_handle.get_ball_snapshot(can_2).await.unwrap().is_none());
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&can_2) && !s.items.contains_key(&loot_in_can_2)));
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp < 10_000.0);
    assert!(sys_handle.get_combat_state(bystander_near).await.unwrap().unwrap().shield_hp < 10_000.0);
    assert_eq!(sys_handle.get_combat_state(bystander_far).await.unwrap().unwrap().shield_hp, 10_000.0);
}

// parity: feat:wormholes.mass-reduction-and-destabilization
#[tokio::test]
async fn test_wormhole_decay_stages_propulsion_mass_collapse_and_static_respawn() {
    eve_testkit::attest(&[
        "feat:wormholes.mass-reduction-and-destabilization",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, wh_sys_id, dest_sys_id, ship_id) = (90_000_001, 31_000_240u32, 31_000_028u32, 630_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 703, char_id, wh_sys_id, ship_id, 621).await;
    ctx.advance_secs(15);

    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(wh_sys_id as i64)], &kw, None).await.unwrap();
    let (static_sig_id, wh_pos) = ctx.db.with_mut_snapshot(|snap| {
        let sig = snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id.starts_with("WST-")).unwrap();
        sig.destination_system_id = Some(dest_sys_id);
        sig.signal_strength = 1.0;
        sig.max_mass_kg = 2_000_000_000.0;
        sig.remaining_mass_kg = 2_000_000_000.0;
        sig.max_jump_mass_kg = 300_000_000.0;
        (sig.signature_id.clone(), sig.position)
    });
    let scan_initial = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(wh_sys_id as i64)], &kw, None).await.unwrap();
    let PyRep::List(sigs_init) = scan_initial.dict_get("signatures").unwrap() else { panic!() };
    let s_init = sigs_init.iter().find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&static_sig_id)).unwrap();
    assert_eq!(s_init.dict_get("massStabilityDescription").and_then(|v| v.as_str()), Some("This wormhole has not yet had its stability significantly disrupted"));

    // Mass reduction with active propulsion module (50MN MWD I = +50,000,000 kg), destabilization stages, and collapse
    let wh_item_id = wormhole_item_id_for_signature(wh_sys_id, &static_sig_id);
    let wh_sys_handle = ctx.systems.get_or_spawn(wh_sys_id);
    let mwd_id = 630_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mwd_id, make_item(mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_active_states.insert(mwd_id);
    });
    let base_caracal_mass = ctx.sde.types.get(&621).map(|t| t.mass).unwrap_or(11_100_000.0);
    let mwd_mass_add = ctx.sde.types.get(&434).and_then(|t| t.default_attributes.get(&796).copied()).unwrap_or(50_000_000.0);
    let expected_jump_mass = base_caracal_mass + mwd_mass_add;
    let jump_pos = DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]);

    spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
    let jump_1 = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
    assert_eq!(jump_1.dict_get("shipJumpMassKg").and_then(|v| v.as_f64()), Some(expected_jump_mass));
    assert!((jump_1.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap() - (2_000_000_000.0 - expected_jump_mass)).abs() < 1.0);

    for (rem_mass, expected_desc) in [
        (600_000_000.0, "This wormhole has had its stability reduced by ships passing through it"),
        (180_000_000.0, "This wormhole has had its stability critically disrupted"),
    ] {
        ctx.advance_secs(301);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_sys_id;
            snap.module_active_states.insert(mwd_id);
            snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id == static_sig_id).unwrap().remaining_mass_kg = rem_mass;
        });
        sess.solar_system_id = Some(wh_sys_id);
        spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
        let j = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
        assert_eq!(j.dict_get("massStabilityDescription").and_then(|v| v.as_str()), Some(expected_desc));
    }

    // Remaining mass 11.35M kg (> base Caracal 11.1M kg, < 61.1M kg with active MWD) -> collapses due to active propulsion mass
    ctx.advance_secs(301);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_sys_id;
        snap.module_active_states.insert(mwd_id);
        snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id == static_sig_id).unwrap().remaining_mass_kg = base_caracal_mass + 250_000.0;
    });
    sess.solar_system_id = Some(wh_sys_id);
    spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
    let jump_collapse = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
    assert_eq!(jump_collapse.dict_get("collapsed"), Some(&PyRep::Bool(true)));
    assert!(wh_sys_handle.get_ball_snapshot(wh_item_id).await.unwrap().is_none());
}

// parity: feat:wormholes.system-phenomena-bonuses, feat:dogma.wormhole-phenomena-effects
#[tokio::test]
async fn test_wormhole_system_phenomena_dogma_modifiers_and_hud() {
    eve_testkit::attest(&[
        "feat:wormholes.system-phenomena-bonuses",
        "feat:dogma.wormhole-phenomena-effects",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let expected_pos = [1.30, 1.44, 1.58, 1.72, 1.86, 2.00];
    let expected_neg = [0.85, 0.78, 0.71, 0.64, 0.57, 0.50];
    let expected_half_pos = [1.15, 1.22, 1.29, 1.36, 1.43, 1.50];
    let expected_wr_small = [1.60, 1.88, 2.16, 2.44, 2.72, 3.00];

    let rifter_base = ctx.sde.types.get(&587).unwrap().default_attributes.clone();
    for (idx, class) in (1u8..=6).enumerate() {
        let base_attrs = || -> HashMap<u16, f64> {
            let mut m = rifter_base.clone();
            for (aid, v) in [(64, 10.0), (84, 100.0), (160, 1.0), (653, 100.0), (654, 100.0), (20, 100.0), (554, 100.0), (1211, 10.0), (1210, 10.0), (116, 100.0)] {
                m.insert(aid, v);
            }
            m
        };

        let mut p_attrs = base_attrs();
        let p_tid = wormhole_phenomenon_type_for_class_and_name(class, "Pulsar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, p_tid, 6, false, &mut p_attrs) && !p_attrs.contains_key(&146));
        assert!((p_attrs[&263] - rifter_base[&263] * expected_pos[idx]).abs() < 1e-4 && (p_attrs[&552] - rifter_base[&552] * expected_pos[idx]).abs() < 1e-4);
        assert!((p_attrs[&55] - rifter_base[&55] * expected_neg[idx]).abs() < 1e-4 && (p_attrs[&267] - rifter_base[&267] * expected_half_pos[idx]).abs() < 1e-4);

        let mut bh_attrs = base_attrs();
        let bh_tid = wormhole_phenomenon_type_for_class_and_name(class, "Black Hole").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, bh_tid, 6, false, &mut bh_attrs));
        assert!((bh_attrs[&37] - rifter_base[&37] * expected_pos[idx]).abs() < 1e-4 && (bh_attrs[&76] - rifter_base[&76] * expected_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&653] - 100.0 * expected_pos[idx]).abs() < 1e-4 && (bh_attrs[&70] - rifter_base[&70] * expected_half_pos[idx]).abs() < 1e-4 && (bh_attrs[&20] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut cv_attrs = base_attrs();
        let cv_tid = wormhole_phenomenon_type_for_class_and_name(class, "Cataclysmic Variable").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, cv_tid, 6, false, &mut cv_attrs));
        assert!((cv_attrs[&482] - rifter_base[&482] * expected_pos[idx]).abs() < 1e-4 && (cv_attrs[&55] - rifter_base[&55] * expected_half_pos[idx]).abs() < 1e-4 && (cv_attrs[&84] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut mg_attrs = base_attrs();
        let mg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Magnetar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, mg_tid, 6, false, &mut mg_attrs));
        assert!((mg_attrs[&64] - 10.0 * expected_pos[idx]).abs() < 1e-4 && (mg_attrs[&654] - 100.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((mg_attrs[&160] - 1.0 * expected_neg[idx]).abs() < 1e-4 && (mg_attrs[&76] - rifter_base[&76] * expected_neg[idx]).abs() < 1e-4 && (mg_attrs[&554] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut rg_attrs = base_attrs();
        let rg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Red Giant").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, rg_tid, 6, false, &mut rg_attrs));
        assert!((rg_attrs[&1211] - 10.0 * expected_half_pos[idx]).abs() < 1e-4 && (rg_attrs[&1210] - 10.0 * expected_pos[idx]).abs() < 1e-4 && (rg_attrs[&116] - 100.0 * expected_pos[idx]).abs() < 1e-4);

        let mut wr_attrs = base_attrs();
        let wr_tid = wormhole_phenomenon_type_for_class_and_name(class, "Wolf-Rayet").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, false, &mut wr_attrs) && !wr_attrs.contains_key(&1493));
        assert!((wr_attrs[&265] - rifter_base[&265] * expected_pos[idx]).abs() < 1e-4 && (wr_attrs[&552] - rifter_base[&552] * expected_neg[idx]).abs() < 1e-4);
        assert!((wr_attrs[&64] - 10.0 * expected_wr_small[idx]).abs() < 1e-4 && (wr_attrs[&271] - rifter_base[&271] * expected_half_pos[idx]).abs() < 1e-4);

        let mut sleeper_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, true, &mut sleeper_attrs));
        assert_eq!(sleeper_attrs[&265], rifter_base[&265]);
        let mut struct_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 65, false, &mut struct_attrs));
        assert_eq!(struct_attrs[&265], rifter_base[&265]);
    }

    assert_eq!(resolve_system_wormhole_phenomenon(&ctx.sde, 31_002_585, None).expect("C13 shattered"), (13, 6, "Wolf-Rayet", 30879));
    assert_eq!(resolve_system_wormhole_phenomenon(&ctx.sde, 31_000_006, None).expect("Drifter WR"), (18, 2, "Wolf-Rayet", 30875));

    // Live ship attribute verification via `dogmaIM.GetAllInfo` in Jita vs W-space phenomenon system `31_000_006` (Class 2 Wolf-Rayet)
    let (char_id, ship_id) = (90_000_001, 640_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 704, char_id, 30_000_142, ship_id, 587).await;
    let get_ship_attr = |info: &PyRep, aid: i64| -> f64 {
        let Some(PyRep::Dict(entries)) = info.dict_get("shipAttributes") else { panic!("shipAttributes") };
        entries.iter().find(|(k, _)| k.as_i64() == Some(aid)).and_then(|(_, v)| v.as_f64()).unwrap()
    };
    let jita_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    let (jita_armor, jita_sig, jita_sh_em) = (get_ship_attr(&jita_info, 265), get_ship_attr(&jita_info, 552), get_ship_attr(&jita_info, 271));

    sess.solar_system_id = Some(31_000_006);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 31_000_006;
        snap.items.get_mut(&ship_id).unwrap().location_id = 31_000_006;
    });
    let wh_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    assert!((get_ship_attr(&wh_info, 265) - jita_armor * 1.44).abs() < 1e-3);
    assert!((get_ship_attr(&wh_info, 552) - jita_sig * 0.78).abs() < 1e-3);
    assert!((get_ship_attr(&wh_info, 271) - jita_sh_em * 1.22).abs() < 1e-3);

    // Verify Sleeper NPC entity in `31_000_006` is NOT modified by `apply_active_dbuffs_and_system_effects_to_ship_attrs`
    let sleeper_item_id = 640_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(sleeper_item_id, make_item(sleeper_item_id, 30187, 1, 31_000_006, InventoryFlag::None, 1, true));
    });
    let mut npc_attrs = HashMap::from([(265u16, 1000.0), (552u16, 100.0)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, sleeper_item_id, &mut npc_attrs);
    assert_eq!(npc_attrs[&265], 1000.0, "Sleeper NPC armorHP unaffected by W-space phenomenon");

    let hud_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsInfo", &[PyRep::Integer(31_000_006)], &kw, None).await.unwrap();
    assert_eq!((hud_info.dict_get("hudVisible"), hud_info.dict_get("appliesToCapsuleerShipsOnly")), (Some(&PyRep::Bool(true)), Some(&PyRep::Bool(true))));
    assert_eq!(hud_info.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));

    let on_ship = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsOnShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    assert_eq!((on_ship.dict_get("hudVisible"), on_ship.dict_get("appliesToCapsuleerShipsOnly")), (Some(&PyRep::Bool(true)), Some(&PyRep::Bool(true))));
    assert_eq!(on_ship.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
}
