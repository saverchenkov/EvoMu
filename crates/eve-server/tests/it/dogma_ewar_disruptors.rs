//! EWAR Tracking Disruptor, Guidance Disruptor, Target Painter, Web/Scram, and Lock Range helper assertions for DG-5.

use crate::dogma_ewar_targeters::extract_item_attr;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, DestinyEvent, MovementMode, TargetLock};
use eve_dogma::*;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_ewar_disruptors_painter_and_locks(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    victim_id: i64,
    damp_mod_id: i64,
    damp_rng_script_id: i64,
    damp_scan_script_id: i64,
    td_mod_id: i64,
    gd_mod_id: i64,
    tp_mod_id: i64,
    opt_script_id: i64,
    trk_script_id: i64,
    m_rng_script_id: i64,
    m_prec_script_id: i64,
    target_turret_id: i64,
    target_launcher_id: i64,
) {
    let kw = HashMap::new();
    // 3. Verify Remote Sensor Dampener & Scripts via dogmaIM.Activate and ItemGetInfo (feat:modules.sensor-dampener)
    let damp_item = DogmaItemState::from_sde(6, 1968, &ctx.sde);
    let (damp_scan_un, damp_range_un) = eval_scripted_sensor_dampener(&damp_item, None, 10_000.0, &ctx.sde);
    assert!(damp_scan_un < 1.0 && damp_range_un < 1.0);
    assert!((damp_scan_un - (1.0 - 0.137)).abs() < 1e-4);
    assert!((damp_range_un - (1.0 - 0.137)).abs() < 1e-4);

    let (damp_scan_scr, damp_range_scr) = eval_scripted_sensor_dampener(&damp_item, Some(29013), 10_000.0, &ctx.sde);
    assert!(damp_scan_scr < damp_scan_un, "Scripted scan dampening must double scan resolution reduction");
    assert_eq!(damp_range_scr, 1.0, "Scan resolution script zeroes range reduction");

    let (damp_scan_rng, damp_range_rng) = eval_scripted_sensor_dampener(&damp_item, Some(29015), 10_000.0, &ctx.sde);
    assert_eq!(damp_scan_rng, 1.0, "Targeting range script zeroes scan res reduction");
    assert!(damp_range_rng < damp_range_un, "Scripted range dampening must double range reduction");

    let base_victim_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo baseline victim ship");
    let base_victim_range = extract_item_attr(&base_victim_info, 76);
    let base_victim_scan = extract_item_attr(&base_victim_info, 564);
    let base_victim_sig = extract_item_attr(&base_victim_info, 552);
    assert!(base_victim_range > 0.0 && base_victim_scan > 0.0 && base_victim_sig > 0.0);

    // 3a. Activate unscripted Remote Sensor Dampener I (1968) against victim_id
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate unscripted Remote Sensor Dampener I");
    let damp_un_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under unscripted Sensor Dampener");
    let victim_range_un = extract_item_attr(&damp_un_info, 76);
    let victim_scan_un = extract_item_attr(&damp_un_info, 564);
    assert_eq!(
        (victim_range_un < base_victim_range, victim_scan_un < base_victim_scan),
        (true, true),
        "Unscripted Remote Sensor Dampener I must reduce victim maxTargetRange (76): {victim_range_un} vs {base_victim_range} and scanResolution (564): {victim_scan_un} vs {base_victim_scan}"
    );
    assert_eq!((victim_range_un / base_victim_range * 1000.0).round() as i64, (damp_range_un * 1000.0).round() as i64);
    assert_eq!((victim_scan_un / base_victim_scan * 1000.0).round() as i64, (damp_scan_un * 1000.0).round() as i64);
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into())],
        &kw, None,
    ).await.expect("Deactivate unscripted Remote Sensor Dampener I");

    // 3b. Load Targeting Range Dampening Script (29015) and activate against victim_id
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(damp_rng_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot4 as i64)],
        &kw, None,
    ).await.expect("Load Targeting Range Dampening Script into Remote Sensor Dampener I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Range-scripted Remote Sensor Dampener I");
    let damp_rng_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Range-scripted Sensor Dampener");
    let victim_range_scr = extract_item_attr(&damp_rng_info, 76);
    let victim_scan_rng_scr = extract_item_attr(&damp_rng_info, 564);
    assert!(victim_range_scr < victim_range_un, "Targeting Range Dampening Script must double maxTargetRange reduction");
    assert!((victim_scan_rng_scr - base_victim_scan).abs() < 1e-4, "Targeting Range Dampening Script must leave scanResolution unpenalized");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into())],
        &kw, None,
    ).await.expect("Deactivate Range-scripted Remote Sensor Dampener I");

    // 3c. Load Scan Resolution Dampening Script (29013) and activate against victim_id
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(damp_scan_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot4 as i64)],
        &kw, None,
    ).await.expect("Load Scan Resolution Dampening Script into Remote Sensor Dampener I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate ScanRes-scripted Remote Sensor Dampener I");
    let damp_scan_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under ScanRes-scripted Sensor Dampener");
    let victim_range_scan_scr = extract_item_attr(&damp_scan_info, 76);
    let victim_scan_scr = extract_item_attr(&damp_scan_info, 564);
    assert!((victim_range_scan_scr - base_victim_range).abs() < 1e-4, "Scan Resolution Dampening Script must leave maxTargetRange unpenalized");
    assert!(victim_scan_scr < victim_scan_un, "Scan Resolution Dampening Script must double scanResolution reduction");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(damp_mod_id), PyRep::String("remoteSensorDampFalloff".into())],
        &kw, None,
    ).await.expect("Deactivate ScanRes-scripted Remote Sensor Dampener I");

    // 4. Verify Tracking Disruptor & Guidance Disruptor with Scripts via dogmaIM.Activate (feat:modules.tracking-disruptor)
    let base_turret_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo baseline turret");
    let base_optimal = extract_item_attr(&base_turret_info, 54);
    let base_falloff = extract_item_attr(&base_turret_info, 158);
    let base_tracking = extract_item_attr(&base_turret_info, 160);
    assert!(base_optimal > 0.0 && base_falloff > 0.0 && base_tracking > 0.0);

    let base_launcher_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo baseline launcher");
    let base_missile_vel = extract_item_attr(&base_launcher_info, 37);
    let base_flight_time = extract_item_attr(&base_launcher_info, 281);
    let base_exp_vel = extract_item_attr(&base_launcher_info, 653);
    let base_exp_radius = extract_item_attr(&base_launcher_info, 654);
    assert!(base_missile_vel > 0.0 && base_flight_time > 0.0 && base_exp_vel > 0.0 && base_exp_radius > 0.0);

    // 4a. Unscripted Tracking Disruptor I (2108) -> reduces turret optimal (54), falloff (158), and trackingSpeed (160)
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate unscripted Tracking Disruptor I");
    let td_unscripted_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under unscripted Tracking Disruptor I");
    let td_un_opt = extract_item_attr(&td_unscripted_info, 54);
    let td_un_fall = extract_item_attr(&td_unscripted_info, 158);
    let td_un_trk = extract_item_attr(&td_unscripted_info, 160);
    assert!(td_un_opt < base_optimal, "Unscripted TD must reduce target turret optimal range");
    assert!(td_un_fall < base_falloff, "Unscripted TD must reduce target turret falloff");
    assert!(td_un_trk < base_tracking, "Unscripted TD must reduce target turret tracking speed");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate Tracking Disruptor I");

    // 4b. Tracking Disruptor I loaded with Optimal Range Disruption Script (29005)
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(opt_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot2 as i64)],
        &kw, None,
    ).await.expect("Load Optimal Range Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Optimal Range scripted Tracking Disruptor I");
    let td_opt_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under Optimal Range scripted TD");
    let td_opt_opt = extract_item_attr(&td_opt_info, 54);
    let td_opt_fall = extract_item_attr(&td_opt_info, 158);
    let td_opt_trk = extract_item_attr(&td_opt_info, 160);
    assert!(td_opt_opt < td_un_opt, "Optimal Range Disruption Script must double optimal range reduction");
    assert!(td_opt_fall < td_un_fall, "Optimal Range Disruption Script must double falloff reduction");
    assert!((td_opt_trk - base_tracking).abs() < 1e-4, "Optimal Range Disruption Script must leave tracking speed unpenalized");
    let unscripted_opt_pen = 1.0 - (td_un_opt / base_optimal);
    let scripted_opt_pen = 1.0 - (td_opt_opt / base_optimal);
    assert!(
        (scripted_opt_pen - 2.0 * unscripted_opt_pen).abs() < 1e-3,
        "Optimal Range Disruption Script must double the unscripted optimal range penalty ({scripted_opt_pen} vs 2*{unscripted_opt_pen})"
    );
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate Optimal Range scripted TD");

    // 4c. Tracking Disruptor I loaded with Tracking Speed Disruption Script (29007)
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(trk_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot2 as i64)],
        &kw, None,
    ).await.expect("Load Tracking Speed Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Tracking Speed scripted Tracking Disruptor I");
    let td_trk_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under Tracking Speed scripted TD");
    let td_trk_opt = extract_item_attr(&td_trk_info, 54);
    let td_trk_fall = extract_item_attr(&td_trk_info, 158);
    let td_trk_trk = extract_item_attr(&td_trk_info, 160);
    assert!((td_trk_opt - base_optimal).abs() < 1e-4, "Tracking Speed Disruption Script must leave optimal range unpenalized");
    assert!((td_trk_fall - base_falloff).abs() < 1e-4, "Tracking Speed Disruption Script must leave falloff unpenalized");
    assert!(td_trk_trk < td_un_trk, "Tracking Speed Disruption Script must double tracking speed reduction");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(td_mod_id), PyRep::String("shipModuleTrackingDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate Tracking Speed scripted TD");

    // 4d. Unscripted Guidance Disruptor I (37543)
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate unscripted Guidance Disruptor I");
    let gd_un_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under unscripted Guidance Disruptor I");
    let gd_un_vel = extract_item_attr(&gd_un_info, 37);
    let gd_un_time = extract_item_attr(&gd_un_info, 281);
    let gd_un_exp_vel = extract_item_attr(&gd_un_info, 653);
    let gd_un_exp_rad = extract_item_attr(&gd_un_info, 654);
    assert!(gd_un_vel < base_missile_vel, "Unscripted Guidance Disruptor must reduce target missile velocity");
    assert!(gd_un_time < base_flight_time, "Unscripted Guidance Disruptor must reduce target missile flight time");
    assert!(gd_un_exp_vel < base_exp_vel, "Unscripted Guidance Disruptor must reduce target missile explosion velocity");
    assert!(gd_un_exp_rad > base_exp_radius, "Unscripted Guidance Disruptor must increase target missile explosion radius");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate unscripted Guidance Disruptor I");

    // 4e. Guidance Disruptor I loaded with Missile Range Disruption Script (40334)
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(m_rng_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot3 as i64)],
        &kw, None,
    ).await.expect("Load Missile Range Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Missile Range scripted Guidance Disruptor I");
    let gd_rng_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under Missile Range scripted GD");
    let gd_rng_vel = extract_item_attr(&gd_rng_info, 37);
    let gd_rng_time = extract_item_attr(&gd_rng_info, 281);
    let gd_rng_exp_vel = extract_item_attr(&gd_rng_info, 653);
    let gd_rng_exp_rad = extract_item_attr(&gd_rng_info, 654);
    assert!(gd_rng_vel < gd_un_vel, "Missile Range Disruption Script must double missile velocity reduction");
    assert!(gd_rng_time < gd_un_time, "Missile Range Disruption Script must double missile flight time reduction");
    assert!((gd_rng_exp_vel - base_exp_vel).abs() < 1e-4, "Missile Range Disruption Script must leave explosion velocity unpenalized");
    assert!((gd_rng_exp_rad - base_exp_radius).abs() < 1e-4, "Missile Range Disruption Script must leave explosion radius unpenalized");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate Missile Range scripted GD");

    // 4f. Guidance Disruptor I loaded with Missile Precision Disruption Script (40335)
    ctx.dispatch_rpc(
        sess, "invbroker", None, "Add",
        &[PyRep::Integer(m_prec_script_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot3 as i64)],
        &kw, None,
    ).await.expect("Load Missile Precision Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Missile Precision scripted Guidance Disruptor I");
    let gd_prec_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under Missile Precision scripted GD");
    let gd_prec_vel = extract_item_attr(&gd_prec_info, 37);
    let gd_prec_time = extract_item_attr(&gd_prec_info, 281);
    let gd_prec_exp_vel = extract_item_attr(&gd_prec_info, 653);
    let gd_prec_exp_rad = extract_item_attr(&gd_prec_info, 654);
    assert!((gd_prec_vel - base_missile_vel).abs() < 1e-4, "Missile Precision Disruption Script must leave missile velocity unpenalized");
    assert!((gd_prec_time - base_flight_time).abs() < 1e-4, "Missile Precision Disruption Script must leave missile flight time unpenalized");
    assert!(gd_prec_exp_vel < gd_un_exp_vel, "Missile Precision Disruption Script must double explosion velocity reduction");
    assert!(gd_prec_exp_rad > gd_un_exp_rad, "Missile Precision Disruption Script must double explosion radius expansion");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(gd_mod_id), PyRep::String("shipModuleGuidanceDisruptor".into())],
        &kw, None,
    ).await.expect("Deactivate Missile Precision scripted GD");

    // 5. Verify Target Painter with Overload via dogmaIM.Activate and ItemGetInfo (feat:modules.target-painter)
    let tp_item = DogmaItemState::from_sde(8, 12709, &ctx.sde);
    let tp_unloaded = eval_target_painter_with_overload(&tp_item, false, 10_000.0, &ctx.sde);
    assert!((tp_unloaded - 1.25).abs() < 1e-4, "Target painter must expand sig radius by 25%");
    let tp_overloaded = eval_target_painter_with_overload(&tp_item, true, 10_000.0, &ctx.sde);
    assert!((tp_overloaded - 1.30).abs() < 1e-4, "Overloaded target painter must expand sig radius by 30%");

    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(tp_mod_id), PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Target Painter I via dogmaIM.Activate");
    let tp_norm_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Target Painter I");
    let victim_sig_norm = extract_item_attr(&tp_norm_info, 552);
    assert!(victim_sig_norm > base_victim_sig * 1.24, "Activated Target Painter I must bloom victim signatureRadius (552) by >=25%");
    assert_eq!((victim_sig_norm / base_victim_sig * 100.0).round() as i64, 125);
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(tp_mod_id), PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into())],
        &kw, None,
    ).await.expect("Deactivate Target Painter I");

    ctx.dispatch_rpc(sess, "dogmaIM", None, "Overload", &[PyRep::Integer(tp_mod_id)], &kw, None).await.expect("Overload Target Painter I");
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(tp_mod_id), PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()), PyRep::Integer(victim_id), PyRep::Integer(1)],
        &kw, None,
    ).await.expect("Activate Overloaded Target Painter I");
    let tp_ovl_info = ctx.dispatch_rpc(sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Overloaded Target Painter I");
    let victim_sig_ovl = extract_item_attr(&tp_ovl_info, 552);
    assert!(victim_sig_ovl > victim_sig_norm, "Overloaded Target Painter I must bloom victim signatureRadius more than non-overloaded");
    assert_eq!((victim_sig_ovl / base_victim_sig * 100.0).round() as i64, 130);
    ctx.dispatch_rpc(
        sess, "dogmaIM", None, "Deactivate",
        &[PyRep::Integer(tp_mod_id), PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into())],
        &kw, None,
    ).await.expect("Deactivate Overloaded Target Painter I");

    // 6. Verify Stasis Webifier & Warp Scrambler (DG-5)
    let web_item = DogmaItemState::from_sde(9, 526, &ctx.sde);
    let slowed_vel = eval_stasis_webifier_stacking(&[&web_item], &[5_000.0], 365.0, &ctx.sde);
    assert!((slowed_vel - 182.5).abs() < 1e-2, "Webifier must reduce velocity by 50%");

    let scram_item = DogmaItemState::from_sde(10, 447, &ctx.sde);
    let scram_state = eval_warp_scrambler_effects(&[&scram_item], &[5_000.0], &ctx.sde);
    assert_eq!(scram_state.scramble_strength, 2);
    assert!(scram_state.blocks_mwd, "Warp scrambler must suppress MWD");

    // 7. Verify Target Lock Time & Max Locked Targets (DG-5)
    let t_lock_fast = compute_target_lock_time(660.0, 100.0);
    let t_lock_slow = compute_target_lock_time(660.0, 35.0);
    assert!(t_lock_slow > t_lock_fast, "Smaller signature radius takes longer to lock");

    assert_eq!(compute_effective_max_locked_targets(4, 4, 0), 4);
    assert_eq!(compute_effective_max_locked_targets(10, 5, 0), 7);
    assert_eq!(compute_effective_max_locked_targets(10, 5, 3), 10);

    // Lock break on range: target moves beyond maxTargetRange
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
    let (src_ball_id, tgt_ball_id) = (990_000_040i64, 990_000_041i64);
    ballpark.add_ball(Ball {
        id: src_ball_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 3.2, mass: 1_350_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(src_ball_id, CombatState::default());
    ballpark.add_ball(Ball {
        id: tgt_ball_id, type_id: 587, position: DVec3::new(20_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 3.2, mass: 1_350_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(tgt_ball_id, CombatState::default());
    ballpark.add_target_lock(src_ball_id, TargetLock {
        target_id: tgt_ball_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(25_000.0),
    });
    ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 1);

    if let Some(tb) = ballpark.balls.get_mut(&tgt_ball_id) {
        tb.position = DVec3::new(30_000.0, 0.0, 0.0);
    }
    let step_events = ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 0, "Lock must break when target exceeds max range");
    assert!(step_events.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, .. } if mode == "lost")));
}
