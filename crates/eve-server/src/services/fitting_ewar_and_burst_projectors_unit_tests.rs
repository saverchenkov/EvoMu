//! Semantic unit tests split from `fitting_ewar_capital_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::inventory_fitting::fitting_ewar_capital::{
    emit_committed_ewar_and_system_wide_notifications, ewar_jamming_type,
};
use eve_db::AccountRecord;
use eve_destiny::{DVec3, MovementMode};
use eve_dogma::{attr_ids, FittedModule, ModuleState, ShipFittingEngine};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn scram_compressor_wdfg_wcs_ewar_grappler_remote_assist_and_burst_projectors() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 97_100_003i64;
    let ship_id = 97_100_104i64;
    let target_id = 97_100_105i64;
    let mod_id = 97_100_204i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 1000.0).await;
    spawn_test_ball(&sys, target_id, 587, DVec3::new(15_000.0, 0.0, 0.0), MovementMode::Stop, 5000.0).await;

    // 1. Warp Scrambler (group 52): missing target, self target, out of range, overloaded in range
    let (scram_fm, scram_st) = synth_module_and_state(mod_id, 448, 19, None, &[(attr_ids::MAX_RANGE, 14_000.0)]);
    let scram_type = synth_type(448, 52, "Warp Scrambler II", &[], &[(1208, 20.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(ship_id), false, false, false)
            .await.err().unwrap().code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );
    // Overloaded increases 14km * 1.2 = 16.8km >= 15km
    let scram_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(target_id), true, false, false,
    ).await.unwrap();
    assert!(scram_ok.warp_scramble_points >= 1);

    // 2. Shipboard Compressor (attr 3265 > 0.0) requires active Industrial Core (group 515)
    let (comp_fm, comp_st) = synth_module_and_state(mod_id, 62000, 27, None, &[]);
    let comp_type = synth_type(62000, 4174, "Medium Industrial Compressor I", &[], &[(3265, 1.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 941, mod_id, &comp_fm, &comp_type, &comp_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "IndustrialCoreNotActive"
    );
    let mut fit_with_core = fit.clone();
    fit_with_core.modules.insert(
        999_002,
        FittedModule {
            item_id: 999_002,
            type_id: 28583,
            flag_id: 28,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    assert!(run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 941, mod_id, &comp_fm, &comp_type, &comp_st, &fit_with_core, None, false, false, false,
    ).await.is_ok());

    // 3. HIC WDFG (group 899): invalid hull, scripted missing target, scripted out of range, scripted ok, unscripted Zarzakh vs normal
    let (wdfg_fm, wdfg_st) = synth_module_and_state(mod_id, 28654, 27, Some(45010), &[]);
    let wdfg_type = synth_type(28654, 899, "Warp Disruption Field Generator I", &[], &[(103, 10_000.0), (5561, 1.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    // Script 45010 gives 1.2 * 10km = 12km < 15km -> TargetOutOfRange; overloaded gives 1.2 * 1.2 * 10km = 14.4km; script 29003 gives 1.5 * 10km = 15km
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );
    let (wdfg_fm_29003, _) = synth_module_and_state(mod_id, 28654, 27, Some(29003), &[]);
    let wdfg_scr_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm_29003, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();
    assert_eq!(wdfg_scr_ok.warp_scramble_points, 100);
    assert!(wdfg_scr_ok.disables_target_mwd_mjd && wdfg_scr_ok.hic_bubble_radius_m.is_none());

    let (wdfg_unscr, _) = synth_module_and_state(mod_id, 28654, 27, None, &[]);
    assert_eq!(
        run_prep(&ctx, &sys, 30_100_000, char_id, ship_id, 894, mod_id, &wdfg_unscr, &wdfg_type, &wdfg_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "ModuleDisallowedInHazardSystem"
    );
    let wdfg_bub_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_unscr, &wdfg_type, &wdfg_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert_eq!(wdfg_bub_ok.hic_bubble_radius_m, Some(10_000.0));
    assert_eq!(wdfg_bub_ok.hic_velocity_multiplier, Some(0.40));

    // 4. Warp Core Stabilizer (group 315)
    let wcs_type = synth_type(10998, 315, "Warp Core Stabilizer I", &[], &[(105, -2.0)]);
    let wcs_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &wdfg_unscr, &wcs_type, &wdfg_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert_eq!(wcs_ok.warp_core_stab_bonus, 2);

    // 5. Webifier (65), Grappler (1672), Target Painter (379)
    let (web_fm, web_st) = synth_module_and_state(
        mod_id,
        526,
        19,
        None,
        &[(attr_ids::MAX_RANGE, 10_000.0), (attr_ids::FALLOFF, 0.0), (2044, 0.0), (20, -60.0)],
    );
    let web_type = synth_type(526, 65, "Stasis Webifier I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, Some(ship_id), false, false, false)
            .await.err().unwrap().code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );

    let (grap_fm, grap_st) = synth_module_and_state(
        mod_id,
        40690,
        19,
        None,
        &[(attr_ids::MAX_RANGE, 1_000.0), (attr_ids::FALLOFF, 9_000.0), (20, -85.0)],
    );
    let grap_type = synth_type(40690, 1672, "Heavy Stasis Grappler I", &[], &[]);
    let grap_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &grap_fm, &grap_type, &grap_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();
    assert_eq!(grap_ok.grappler_optimal_m, 1_000.0);
    assert_eq!(grap_ok.grappler_falloff_m, 9_000.0);
    assert!(grap_ok.target_velocity_multiplier.unwrap() < 1.0);

    let tp_type = synth_type(12709, 379, "Target Painter I", &[], &[]);
    let _ = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &grap_fm, &tp_type, &grap_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();

    // 6. Remote Assistance (41, 325, 2018, 585, 67)
    let (rr_fm, rr_st) = synth_module_and_state(
        mod_id,
        49722,
        27,
        None,
        &[(attr_ids::MAX_RANGE, 30_000.0), (68, 200.0), (84, 300.0), (83, 150.0), (90, 120.0)],
    );
    let mut muta_fm = rr_fm.clone();
    muta_fm.dynamic_attribute_overrides.insert(84, 450.0);
    let muta_type = synth_type(49722, 2018, "Mutadaptive Remote Armor Repairer", &[], &[(2796, 0.15), (2797, 2.50)]);
    let muta_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &muta_fm, &muta_type, &rr_st, &fit, Some(target_id), true, false, false,
    ).await.unwrap();
    assert!((muta_ok.mutadaptive_spool_step - 0.15).abs() < 1e-6);
    assert!((muta_ok.mutadaptive_spool_max - 2.50).abs() < 1e-6);

    for (gid, check) in [(41u32, 1), (585, 2), (67, 3)] {
        let rt = synth_type(5000 + gid, gid, "Remote Assist", &[], &[]);
        let r_ok = run_prep(
            &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &rr_fm, &rt, &rr_st, &fit, Some(target_id), false, false, false,
        ).await.unwrap();
        match check {
            1 => assert!((r_ok.remote_shield_amount - 200.0).abs() < 1e-6),
            2 => assert!((r_ok.remote_hull_amount - 150.0).abs() < 1e-6),
            _ => assert!((r_ok.remote_cap_amount - 120.0).abs() < 1e-6),
        }
    }

    // Disallow assistance check on target
    let _ = sys
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id: target_id,
            module_id: 888_001,
            duration_s: 60.0,
            self_disallow_assistance: true,
            ..Default::default()
        })
        .await;
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &muta_fm, &muta_type, &rr_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetDisallowsAssistance"
    );

    // 7. Supercarrier Burst Projectors (group 842)
    for b_tid in [27678u32, 40636, 40635, 40696, 40697, 40698, 40699] {
        let (bfm, bst) = synth_module_and_state(mod_id, b_tid, 27, None, &[(2279, 20_000.0), (97, 1500.0), (20, -40.0)]);
        let btype = synth_type(b_tid, 842, "Burst Projector", &[], &[]);
        let bok = run_prep(
            &ctx, &sys, sys_id, char_id, ship_id, 659, mod_id, &bfm, &btype, &bst, &fit, Some(target_id), false, false, false,
        ).await.unwrap();
        if b_tid == 40698 {
            assert!((bok.target_velocity_multiplier.unwrap() - 0.60).abs() < 1e-6);
        }
    }
    assert!(sys.get_combat_state(target_id).await.unwrap().unwrap().capacitor < 5000.0);

    // 8. ewar_jamming_type and emit_committed_ewar_and_system_wide_notifications extra branches
    assert_eq!(ewar_jamming_type(&grap_type, None, false, None), Some("webify"));
    let gd_by_name = synth_type(37541, 291, "Guidance Disruptor II", &[], &[]);
    assert_eq!(ewar_jamming_type(&gd_by_name, None, false, None), Some("ewGuidanceDisrupt"));
    let non_ewar = synth_type(99999, 9999, "Passive Hull Mod", &[], &[]);
    assert_eq!(ewar_jamming_type(&non_ewar, None, false, None), None);
    emit_committed_ewar_and_system_wide_notifications(
        &ctx, sys_id, char_id, ship_id, mod_id, 99999, &non_ewar, None, false, None, Some(target_id),
    );
    let _ = AccountRecord {
        user_id: 1,
        username: String::new(),
        password_token: String::new(),
        role: 0,
        banned_until_unix: None,
        ban_reason: None,
    };
    let _ = PyRep::None;
}
