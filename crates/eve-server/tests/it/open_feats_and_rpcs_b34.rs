#![allow(unused_imports)]
//! End-to-End Integration Tests for Batch 34 `feat:modules.*` parity rows.

use crate::modules_advanced_tactical::*;
use eve_db::{FleetRecord, InventoryFlag};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn b34_auto_targeting_and_cargo_expanders() {
    let ctx = build_ctx();
    let mut sess = seed_pilot(&ctx, 94001, 84001, 621, NULL_SYS, true); // Caracal (621)
    let base_info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
        .await
        .unwrap();
    let base_cargo = attr_from_all_info(&base_info, "shipInfo", 38);
    let base_vel = attr_from_all_info(&base_info, "shipInfo", 37);
    let base_hull = attr_from_all_info(&base_info, "shipInfo", 9);

    // Fit Auto Targeting System I (1182), Expanded Cargohold I (1317), and Medium Cargohold Optimization I (31119)
    fit_mod(&ctx, &mut sess, 84001, 74001, 1182, 27).await.expect("fit Auto Targeting System I");
    fit_mod(&ctx, &mut sess, 84001, 74002, 1317, 11).await.expect("fit Expanded Cargohold I");
    fit_mod(&ctx, &mut sess, 84001, 74003, 31119, 92).await.expect("fit Medium Cargohold Optimization I");

    let mod_info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
        .await
        .unwrap();
    assert!(attr_from_all_info(&mod_info, "shipInfo", 38) > base_cargo * 1.35, "Expanded Cargohold + rig must increase cargo capacity");
    assert!(attr_from_all_info(&mod_info, "shipInfo", 37) < base_vel, "Cargo expander/rig must reduce maxVelocity");
    assert!(attr_from_all_info(&mod_info, "shipInfo", 9) < base_hull, "Expanded Cargohold must reduce hull HP");

    sess.station_id = None;
    spawn_in_space(&ctx, 94001, 84001, 621, NULL_SYS, DVec3::ZERO).await;
    let _hostile = seed_pilot(&ctx, 94002, 84002, 587, NULL_SYS, false);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&94002) {
            c.alliance_id = Some(99000099);
        }
    });
    spawn_in_space(&ctx, 94002, 84002, 587, NULL_SYS, DVec3::new(15_000.0, 0.0, 0.0)).await;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.add_limited_engagement(94001, 94002, ctx.now_unix() + 900);
    });

    let act = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74001), PyRep::String("default".into())])
        .await
        .expect("activate Auto Targeting System I");
    assert_eq!(act.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert!(act.dict_get("maxLockedTargets").and_then(|v| v.as_i64()).unwrap_or(0) >= 8);
    assert_eq!(act.dict_get("lockingCount").and_then(|v| v.as_i64()), Some(1));
}

#[tokio::test]
async fn b34_bastion_effects_and_wormhole_mjd_interaction() {
    let ctx = build_ctx();
    let mut sess = seed_pilot(&ctx, 94010, 84010, 28659, NULL_SYS, true); // Paladin (28659, Marauder)
    fit_mod(&ctx, &mut sess, 84010, 74011, 33400, 27).await.expect("fit Bastion Module I");
    fit_mod(&ctx, &mut sess, 84010, 74012, 4383, 19).await.expect("fit Large MJD");
    sess.station_id = None;
    spawn_in_space(&ctx, 94010, 84010, 28659, NULL_SYS, DVec3::ZERO).await;

    let act = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74011), PyRep::String("default".into())])
        .await
        .expect("activate Bastion");
    assert_eq!(act.dict_get("active").and_then(|v| v.as_bool()), Some(true));

    let info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
        .await
        .unwrap();
    assert_eq!(attr_from_all_info(&info, "shipInfo", 2112), 1.0, "Bastion must NOT grant ECM immunity");
    assert!((attr_from_all_info(&info, "shipInfo", 2135) - 0.05).abs() < 1e-6, "Bastion remoteAssistanceImpedance = 0.05");
    assert_eq!(attr_from_all_info(&info, "shipInfo", 2045), 0.0, "Bastion blocks incoming remote repair");

    // MJD activation is refused while Bastion is active
    let mjd_err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74012), PyRep::String("default".into())])
        .await
        .unwrap_err();
    assert_eq!(mjd_err.code, "WarpScrambled");

    // Command Destroyer MJFG still moves the bastioned Marauder
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let mut cd_sess = seed_pilot(&ctx, 94011, 84011, 37483, NULL_SYS, true); // Pontifex (37483)
    fit_mod(&ctx, &mut cd_sess, 84011, 74013, 37479, 19).await.expect("fit MJFG");
    cd_sess.station_id = None;
    spawn_in_space(&ctx, 94011, 84011, 37483, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let moved = sys.micro_jump_field(84011, 100_000.0, 6_000.0, 25).await.expect("MJFG boosh");
    assert!(moved.contains(&84010), "MJFG must move bastioned Marauder");

    // Bastioned Marauder cannot jump a wormhole (even from 10 km away) and Bastion stays active
    let wh_err = rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", vec![PyRep::Integer(750_000_001)])
        .await
        .unwrap_err();
    assert_eq!(wh_err.code, "CannotJumpWormholeWhileImmobilized");
    let still_active = ctx.db.with_snapshot(|s| s.module_active_states.contains(&74011));
    assert!(still_active, "Bastion module must remain active when WormholeJump is rejected");
}

#[tokio::test]
async fn b34_cehe_and_capital_mjd_mjfg() {
    let ctx = build_ctx();
    // Subcapital cannot activate CEHE
    let mut sub_sess = seed_pilot(&ctx, 94020, 84020, 621, NULL_SYS, true);
    give_item(&ctx, 74021, 40714, 94020, 84020, InventoryFlag::LoSlot0, 1);
    sub_sess.station_id = None;
    spawn_in_space(&ctx, 94020, 84020, 621, NULL_SYS, DVec3::ZERO).await;
    assert_eq!(
        rpc(&ctx, &mut sub_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74021), PyRep::String("default".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidShipGroup"
    );

    // Thanatos (23911, Carrier) activates CEHE (40714) and Capital MJD (83465)
    let mut rev_sess = seed_pilot(&ctx, 94021, 84021, 23911, NULL_SYS, true);
    fit_mod(&ctx, &mut rev_sess, 84021, 74022, 40714, 11).await.expect("fit CEHE I");
    fit_mod(&ctx, &mut rev_sess, 84021, 74023, 83465, 19).await.expect("fit Capital MJD");
    rev_sess.station_id = None;
    spawn_in_space(&ctx, 94021, 84021, 23911, NULL_SYS, DVec3::ZERO).await;

    let cehe_act = rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74022), PyRep::String("default".into())])
        .await
        .expect("activate CEHE I");
    assert_eq!(cehe_act.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    let info = rpc(&ctx, &mut rev_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
        .await
        .unwrap();
    assert!(attr_from_all_info(&info, "shipInfo", 113) <= 0.05 + 1e-6, "CEHE must grant massive hull EM resistance");

    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    // Activate Capital MJD (83465) via dogmaIM.Activate and advance cycle ticks (B34-F-01)
    rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74023), PyRep::String("default".into())])
        .await
        .expect("activate Capital MJD");
    let _ = sys.step_manual(180.0).await;
    let cap_pos = sys.get_ball_snapshot(84021).await.unwrap().unwrap().1.position;
    assert!((cap_pos.length() - 250_000.0).abs() < 100.0, "Capital MJD must jump 250 km forward via cycle, got {cap_pos:?}");

    // Capital MJFG (83485) on Thanatos (23911, Carrier) jumps subcapitals within 10 km 250 km forward via cycle
    if let Ok(Some(mut cs)) = sys.get_combat_state(84020).await {
        cs.undock_invulnerable_remaining_ticks = 0;
        cs.gate_cloak_remaining_ticks = 0;
        let _ = sys.set_combat_state(84020, cs).await;
    }
    let mut car_sess = seed_pilot(&ctx, 94022, 84022, 23911, NULL_SYS, true);
    fit_mod(&ctx, &mut car_sess, 84022, 74024, 83485, 19).await.expect("fit Capital MJFG");
    car_sess.station_id = None;
    spawn_in_space(&ctx, 94022, 84022, 23911, NULL_SYS, DVec3::ZERO).await;
    rpc(&ctx, &mut car_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74024), PyRep::String("default".into())])
        .await
        .expect("activate Capital MJFG");
    let _ = sys.step_manual(180.0).await;
    let mjfg_pos = sys.get_ball_snapshot(84022).await.unwrap().unwrap().1.position;
    let sub_pos = sys.get_ball_snapshot(84020).await.unwrap().unwrap().1.position;
    let cap_other_pos = sys.get_ball_snapshot(84021).await.unwrap().unwrap().1.position;
    assert!((mjfg_pos.length() - 250_000.0).abs() < 100.0, "Capital MJFG caster must jump 250 km forward");
    assert!(sub_pos.x > 240_000.0, "Capital MJFG must jump subcapital within 10 km, got sub_pos={sub_pos:?}");
    assert!((cap_other_pos.length() - 250_000.0).abs() < 100.0, "Capital MJFG must NOT jump other capital ships");
}

#[tokio::test]
async fn b34_cloak_stabilization_and_clone_vat_bay_fuel() {
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let mut cov_sess = seed_pilot(&ctx, 94030, 84030, 11957, NULL_SYS, true);
    fit_mod(&ctx, &mut cov_sess, 84030, 74030, 11578, 27).await.expect("fit Covert Ops Cloaking Device II");
    cov_sess.station_id = None;
    spawn_in_space(&ctx, 94030, 84030, 11957, NULL_SYS, DVec3::new(5_000_000_000.0, 0.0, 0.0)).await;

    // Anchor a real Mobile Observatory (10m deploy + 10m ping interval) > 2km away to avoid proximity decloak
    let mut obs_sess = seed_pilot(&ctx, 94039, 84039, 587, NULL_SYS, true);
    let obs_id = 74039_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(94039).or_default().insert(
            3392,
            eve_db::TrainedSkillRecord {
                type_id: 3392,
                level: 3,
                skill_points: 10_000.0,
            },
        );
    });
    give_item(
        &ctx,
        obs_id,
        eve_server::services::mobile_deployables::TYPE_MOBILE_OBSERVATORY,
        94039,
        84039,
        InventoryFlag::Cargo,
        1,
    );
    obs_sess.station_id = None;
    spawn_in_space(&ctx, 94039, 84039, 587, NULL_SYS, DVec3::new(5_100_000_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut obs_sess, "posMgr", "AnchorStructure", vec![PyRep::Integer(obs_id)])
        .await
        .expect("anchor Mobile Observatory");

    // Activate cloak through dogmaIM.Activate RPC and assert cloak_stabilization_remaining_s (B34-F-02)
    rpc(&ctx, &mut cov_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
        .await
        .expect("activate Covert Ops Cloak");
    let cs = sys.get_combat_state(84030).await.unwrap().unwrap();
    assert!(cs.is_cloaked, "Ship must be cloaked after Activate");
    assert!((cs.cloak_stabilization_remaining_s - 900.0).abs() < 1.0, "Cloak stabilization timer must start at 900s");

    // Advance 601s so the Mobile Observatory finishes 10m deploy and fires its first decloak ping;
    // ship has only been cloaked ~601s (< 900s stabilization timer) so it must remain cloaked
    ctx.advance_virtual_clock(601);
    for _ in 0..10 {
        sys.step_manual(60.1).await.unwrap();
    }
    let cs_mid = sys.get_combat_state(84030).await.unwrap().unwrap();
    assert!(cs_mid.is_cloaked, "Ship must be immune to observatory ping while stabilization timer > 0");
    assert!(cs_mid.cloak_stabilization_remaining_s > 0.0, "Stabilization timer must still be positive at 601s");

    // Deactivating cloak resets cloak_stabilization_remaining_s to 0.0
    rpc(&ctx, &mut cov_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
        .await
        .expect("deactivate Covert Ops Cloak");
    let cs_decloaked = sys.get_combat_state(84030).await.unwrap().unwrap();
    assert!(!cs_decloaked.is_cloaked, "Ship must be decloaked");
    assert_eq!(cs_decloaked.cloak_stabilization_remaining_s, 0.0, "Decloak must reset stabilization timer to 0");

    // Re-cloak and advance virtual clock + simulation past 900s -> real observatory ping decloaks ship
    ctx.advance_virtual_clock(35);
    rpc(&ctx, &mut cov_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
        .await
        .expect("re-activate Covert Ops Cloak");
    ctx.mechanic_rng.set_seed(Some(42));
    ctx.advance_virtual_clock(3_000);
    for _ in 0..50 {
        sys.step_manual(60.0).await.unwrap();
    }
    let cs_after = sys.get_combat_state(84030).await.unwrap().unwrap();
    assert!(!cs_after.is_cloaked, "Expired stabilization timer (>900s) must allow real observatory ping to decloak");

    // Clone Vat Bay I (23735) on Rorqual (28352) requires Liquid Ozone, immobilizes ship, and gates clone install on active cycle
    let mut rorq_sess = seed_pilot(&ctx, 94031, 84031, 28352, NULL_SYS, true);
    let mut guest_sess = seed_pilot(&ctx, 94032, 84032, 621, NULL_SYS, false);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5001, FleetRecord {
            fleet_id: 5001,
            boss_id: 94031,
            free_move: true,
            members: vec![94031, 94032],
            ..Default::default()
        });
    });
    rorq_sess.fleet_id = Some(5001);
    guest_sess.fleet_id = Some(5001);
    fit_mod(&ctx, &mut rorq_sess, 84031, 74031, 23735, 27).await.expect("fit Clone Vat Bay I");
    rorq_sess.station_id = None;
    spawn_in_space(&ctx, 94031, 84031, 28352, NULL_SYS, DVec3::ZERO).await;

    // Fails without Liquid Ozone
    assert_eq!(
        rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74031), PyRep::String("default".into())])
            .await
            .unwrap_err()
            .code,
        "NotEnoughFuel"
    );
    give_item(&ctx, 74032, 16273, 94031, 84031, InventoryFlag::Cargo, 600);
    let cvb_act = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74031), PyRep::String("default".into())])
        .await
        .expect("activate Clone Vat Bay I");
    assert_eq!(cvb_act.dict_get("cloneVatBayActive").and_then(|v| v.as_bool()), Some(true));
    assert!(cvb_act.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // Offering clone succeeds while active
    rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(94032), PyRep::Integer(84031)])
        .await
        .expect("offer clone while active");
    // Deactivating Clone Vat Bay blocks subsequent clone installation
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&74031);
    });
    assert_eq!(
        rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(94032), PyRep::Integer(84031)])
            .await
            .unwrap_err()
            .code,
        "NoActiveCloneVatBay"
    );
}

#[tokio::test]
async fn b34_command_bursts_strength_range_duration_fitting() {
    let ctx = build_ctx();
    // Alpha clone cannot activate Command Burst
    let mut alpha_sess = seed_pilot(&ctx, 94040, 84040, 22474, NULL_SYS, true); // Damnation (22474, Command Ship)
    fit_mod(&ctx, &mut alpha_sess, 84040, 74041, 42526, 27).await.expect("fit Armor Command Burst I");
    give_item(&ctx, 74042, 42832, 94040, 84040, InventoryFlag::HiSlot0, 10);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&94040) {
            c.clone_grade = 0;
        }
    });
    alpha_sess.station_id = None;
    spawn_in_space(&ctx, 94040, 84040, 22474, NULL_SYS, DVec3::ZERO).await;
    assert_eq!(
        rpc(&ctx, &mut alpha_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74041), PyRep::String("default".into())])
            .await
            .unwrap_err()
            .code,
        "OmegaRequiredForCommandBurst"
    );

    // Omega Damnation + Command Processor I rig (11014) + Mindlink + Alpha fleet member in range
    let mut cmd_sess = seed_pilot(&ctx, 94041, 84041, 22474, NULL_SYS, true);
    let mut mem_sess = seed_pilot(&ctx, 94042, 84042, 621, NULL_SYS, false);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&94042) {
            c.clone_grade = 0; // Alpha fleet member still receives effect
        }
        s.fleets.insert(5002, FleetRecord {
            fleet_id: 5002,
            boss_id: 94041,
            free_move: true,
            members: vec![94041, 94042],
            ..Default::default()
        });
    });
    cmd_sess.fleet_id = Some(5002);
    mem_sess.fleet_id = Some(5002);
    fit_mod(&ctx, &mut cmd_sess, 84041, 74043, 43552, 27).await.expect("fit Armor Command Burst II");
    give_item(&ctx, 74044, 42832, 94041, 84041, InventoryFlag::HiSlot0, 10);
    fit_mod(&ctx, &mut cmd_sess, 84041, 74045, 11014, 19).await.expect("fit Command Processor I");
    cmd_sess.station_id = None;
    spawn_in_space(&ctx, 94041, 84041, 22474, NULL_SYS, DVec3::ZERO).await;
    spawn_in_space(&ctx, 94042, 84042, 621, NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;

    let burst = rpc(&ctx, &mut cmd_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74043), PyRep::String("default".into())])
        .await
        .expect("activate Command Burst");
    let aoe_range = burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0);
    // 15km * 2.0 (Command Ship) * (1+0.50 Wing Cmd) * (1+0.25 Fleet Cmd) = 56,250.0 m
    assert!((aoe_range - 56_250.0).abs() < 1.0, "Expected Command Ship burst range 56250.0m, got {aoe_range}");
    assert_eq!(burst.dict_get("reloadTimeSeconds").and_then(|v| v.as_f64()), Some(30.0));
    assert!(burst.dict_get("buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 90.0);
    assert!(burst.dict_get("buffStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > 2.0);
}

#[tokio::test]
async fn b34_cyno_covert_cyno_portal_jammer_and_lance() {
    let ctx = build_ctx();
    let dest_sys = 30004760u32;
    let mut cyno_sess = seed_pilot(&ctx, 94050, 84050, 11957, dest_sys, true); // Falcon (11957)
    let mut blops_sess = seed_pilot(&ctx, 94051, 84051, 22428, NULL_SYS, true); // Redeemer (22428)
    let mut cov_sess = seed_pilot(&ctx, 94052, 84052, 12032, NULL_SYS, false); // Manticore (12032, Stealth Bomber)
    let mut non_cov_sess = seed_pilot(&ctx, 94053, 84053, 621, NULL_SYS, false); // Caracal (621, non-covert)

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5003, FleetRecord {
            fleet_id: 5003,
            boss_id: 94051,
            free_move: true,
            members: vec![94050, 94051, 94052, 94053],
            ..Default::default()
        });
    });
    for s in [&mut cyno_sess, &mut blops_sess, &mut cov_sess, &mut non_cov_sess] {
        s.fleet_id = Some(5003);
    }

    // Tenebrex Cyno Jammer spool-up (300s) and reinforcement disable
    let now_u = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(998_101, eve_db::UpwellStructureRecord {
            structure_id: 998_101,
            type_id: 32458,
            owner_corp_id: 1000125,
            solar_system_id: dest_sys,
            name: "Sov Hub".into(),
            position: [10_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        s.sov_runtime.sov_hubs.insert(
            998_101,
            eve_db::PersistedSovHubData {
                hub_id: 998_101,
                upgrades: vec![eve_db::PersistedHubUpgrade {
                    type_id: eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        s.structures.insert(998_100, eve_db::UpwellStructureRecord {
            structure_id: 998_100,
            type_id: 37534,
            owner_corp_id: 1000125,
            solar_system_id: dest_sys,
            name: "Tenebrex".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        s.sov_runtime.tenebrex_spool_ready_unix.insert(998_100, now_u + 300);
    });
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, dest_sys).is_none(), "Tenebrex must spool up for 300s");
    ctx.advance_virtual_clock(305);
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, dest_sys).is_some(), "Tenebrex must jam after 300s");

    // Covert cyno (28646) can be lit in jammed system, uses 25 LO, not visible on map
    fit_mod(&ctx, &mut cyno_sess, 84050, 74051, 28646, 27).await.expect("fit Covert Cyno");
    give_item(&ctx, 74052, 16273, 94050, 84050, InventoryFlag::Cargo, 100);
    cyno_sess.station_id = None;
    spawn_in_space(&ctx, 94050, 84050, 11957, dest_sys, DVec3::ZERO).await;
    let cov_cyno = rpc(&ctx, &mut cyno_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74051), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("light covert cyno in jammed system");
    let beacon_id = cov_cyno.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cov_cyno.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(5));
    let map_counts = rpc(&ctx, &mut cyno_sess, "map", "GetBeaconCount", vec![]).await.unwrap();
    if let PyRep::Dict(entries) = map_counts {
        assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(dest_sys as i64)), "Covert cyno must NOT appear on map");
    }

    // Covert Jump Portal Generator (28652) on Redeemer bridges covert hull and rejects non-covert hull
    fit_mod(&ctx, &mut blops_sess, 84051, 74053, 28652, 27).await.expect("fit Covert Jump Portal");
    give_item(&ctx, 74054, 16273, 94051, 84051, InventoryFlag::Cargo, 1_000);
    give_item(&ctx, 74055, 16274, 94051, 84051, InventoryFlag::Cargo, 50_000); // Helium Isotopes
    blops_sess.station_id = None;
    spawn_in_space(&ctx, 94051, 84051, 22428, NULL_SYS, DVec3::ZERO).await;
    spawn_in_space(&ctx, 94052, 84052, 12032, NULL_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
    spawn_in_space(&ctx, 94053, 84053, 621, NULL_SYS, DVec3::new(500.0, 0.0, 0.0)).await;

    let b_id = ctx.bind_object(&mut blops_sess, "beyonce", NULL_SYS as i64).unwrap();
    ctx.dispatch_rpc(&mut blops_sess, "beyonce", Some(b_id), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(94050), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None)
        .await
        .expect("open covert bridge");

    let non_cov_b = ctx.bind_object(&mut non_cov_sess, "beyonce", NULL_SYS as i64).unwrap();
    let rej = ctx.dispatch_rpc(&mut non_cov_sess, "beyonce", Some(non_cov_b), "CmdJumpThroughFleet", &[PyRep::Integer(94051), PyRep::Integer(84051)], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(rej.code, "JumpBridgeRestrictedShip");

    let cov_b = ctx.bind_object(&mut cov_sess, "beyonce", NULL_SYS as i64).unwrap();
    ctx.dispatch_rpc(&mut cov_sess, "beyonce", Some(cov_b), "CmdJumpThroughFleet", &[PyRep::Integer(94051), PyRep::Integer(84051)], &HashMap::new(), None)
        .await
        .expect("covert ship jumps through covert portal");

    // Disruptive Lance (77398) on Valravn (77288, Lancer Dreadnought)
    let mut bane_sess = seed_pilot(&ctx, 94054, 84054, 77288, NULL_SYS, true);
    fit_mod(&ctx, &mut bane_sess, 84054, 74056, 77398, 27).await.expect("fit Disruptive Lance");
    fit_mod(&ctx, &mut bane_sess, 84054, 74057, 20280, 28).await.expect("fit Siege Module I");
    give_item(&ctx, 74058, 16275, 94054, 84054, InventoryFlag::Cargo, 500);
    give_item(&ctx, 74059, 17889, 94054, 84054, InventoryFlag::Cargo, 50_000);
    bane_sess.station_id = None;
    spawn_in_space(&ctx, 94054, 84054, 77288, NULL_SYS, DVec3::ZERO).await;
    rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74057), PyRep::String("default".into())])
        .await
        .expect("activate Siege");
    let lance = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74056), PyRep::String("default".into())])
        .await
        .expect("activate Disruptive Lance");
    assert!((lance.dict_get("totalDamageApplied").and_then(|v| v.as_f64()).unwrap_or(0.0) - 382_500.0).abs() < 1.0);
    assert_eq!(lance.dict_get("neutralizerAmountGJ").and_then(|v| v.as_f64()), Some(31_000.0));
}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod part2 {
    use super::*;

// Batch 34 Part 2: Normal Cyno Lighting Rules, Tenebrex Jammer State/Reinforcement,
// and Strengthened Bastion & Command Burst Assertions.

use crate::modules_advanced_tactical::*;
use eve_db::{FleetRecord, InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn b34_normal_cyno_lighting_rules() {
    let ctx = build_ctx();

    // 1. Not in fleet (when another fleet exists on server) -> MustBeInFleet
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5100, FleetRecord {
            fleet_id: 5100,
            boss_id: 94199,
            free_move: true,
            members: vec![94199],
            ..Default::default()
        });
    });
    let mut solo_sess = seed_pilot(&ctx, 94101, 84101, 11957, NULL_SYS, true);
    fit_mod(&ctx, &mut solo_sess, 84101, 74101, 21096, 27).await.expect("fit Cynosural Field Generator I");
    give_item(&ctx, 74102, 16273, 94101, 84101, InventoryFlag::Cargo, 1_000);
    solo_sess.station_id = None;
    spawn_in_space(&ctx, 94101, 84101, 11957, NULL_SYS, DVec3::ZERO).await;

    let err_no_fleet = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_no_fleet.code, "MustBeInFleet", "Normal cyno must require fleet when fleets exist");

    // 2. High-sec system (30000142 Jita) -> CannotLightCynoInSystem
    let mut hs_sess = seed_pilot(&ctx, 94102, 84102, 11957, 30_000_142, true);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5101, FleetRecord {
            fleet_id: 5101,
            boss_id: 94102,
            free_move: true,
            members: vec![94102, 94101],
            ..Default::default()
        });
    });
    hs_sess.fleet_id = Some(5101);
    fit_mod(&ctx, &mut hs_sess, 84102, 74103, 21096, 27).await.expect("fit cyno in highsec");
    give_item(&ctx, 74104, 16273, 94102, 84102, InventoryFlag::Cargo, 1_000);
    hs_sess.station_id = None;
    spawn_in_space(&ctx, 94102, 84102, 11957, 30_000_142, DVec3::ZERO).await;
    let err_hs = rpc(&ctx, &mut hs_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74103), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_hs.code, "CannotLightCynoInSystem");

    // 3. Speed >= 500 m/s -> ShipMovingTooFast
    solo_sess.fleet_id = Some(5101);
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let _ = sys.spawn_ball(Ball {
        id: 84101,
        type_id: 621,
        position: DVec3::ZERO,
        velocity: DVec3::new(600.0, 0.0, 0.0),
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.6,
        mode: MovementMode::Stop,
    }).await;
    let err_fast = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_fast.code, "ShipMovingTooFast");

    // Stop ship
    let _ = sys.spawn_ball(Ball {
        id: 84101,
        type_id: 621,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    // 4. Insufficient Liquid Ozone (< 50 LO on Force Recon) -> NotEnoughFuel
    ctx.db.with_mut_snapshot(|s| {
        if let Some(lo) = s.items.get_mut(&74102) {
            lo.quantity = 40;
        }
    });
    let err_lo = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_lo.code, "NotEnoughFuel");

    // 5. Valid normal cyno activation: consumes 50 LO (Force Recon 90% reduction from 500 LO), 5-minute (300s) cycle
    ctx.db.with_mut_snapshot(|s| {
        if let Some(lo) = s.items.get_mut(&74102) {
            lo.quantity = 100;
        }
    });
    let ok_cyno = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("light normal cyno");
    assert_eq!(ok_cyno.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(50));
    let rem_lo = ctx.db.with_snapshot(|s| s.items.get(&74102).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_lo, 50, "Normal cyno must consume 50 Liquid Ozone on Force Recon");
}

#[tokio::test]
async fn b34_tenebrex_jammer_state_and_reinforcement() {
    let ctx = build_ctx();
    let jam_sys = 30_004_761u32;
    let mut sess = seed_pilot(&ctx, 94110, 84110, 621, jam_sys, true);
    sess.station_id = None;
    spawn_in_space(&ctx, 94110, 84110, 621, jam_sys, DVec3::ZERO).await;

    let now_u = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(998_200, UpwellStructureRecord {
            structure_id: 998_200,
            type_id: 37534,
            owner_corp_id: 1000125,
            solar_system_id: jam_sys,
            name: "Tenebrex".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        s.sov_runtime.tenebrex_spool_ready_unix.insert(998_200, now_u + 300);
    });

    // Hubless Tenebrex must NOT jam even after spool
    ctx.advance_virtual_clock(305);
    assert!(
        eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none(),
        "Hubless Tenebrex must NOT jam cynos"
    );

    // Seed Sovereignty Hub with Cynosural Suppression upgrade + fresh 300s spool
    let now_u2 = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(998_201, UpwellStructureRecord {
            structure_id: 998_201,
            type_id: 32458,
            owner_corp_id: 1000125,
            solar_system_id: jam_sys,
            name: "Sov Hub".into(),
            position: [10_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        s.sov_runtime.sov_hubs.insert(
            998_201,
            eve_db::PersistedSovHubData {
                hub_id: 998_201,
                upgrades: vec![eve_db::PersistedHubUpgrade {
                    type_id: eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        s.sov_runtime.tenebrex_spool_ready_unix.insert(998_200, now_u2 + 300);
    });

    // Spooling up -> not active yet
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none());

    // After 300s spool -> active
    ctx.advance_virtual_clock(305);
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_some());

    let b_id = ctx.bind_object(&mut sess, "beyonce", jam_sys as i64).unwrap();
    let state_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
        .await
        .expect("GetCynoJammerState");
    assert!(matches!(state_rep, PyRep::Integer(ts) if ts > 0), "GetCynoJammerState must return online timestamp when jammed, got {state_rep:?}");

    // Armor/hull reinforcement disables Tenebrex cyno jammer
    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&998_200) {
            st.state = "ArmorReinforced".into();
            st.reinforced_until_unix = ctx.now_unix_secs() + 86_400;
        }
    });
    assert!(
        eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none(),
        "Reinforced Tenebrex must NOT jam cynos"
    );
    let state_after = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
        .await
        .expect("GetCynoJammerState after reinforcement");
    assert_eq!(state_after, PyRep::None, "GetCynoJammerState must return None when reinforced");
}

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod w34_closure {
    use super::*;

// Live integration tests for newly-closed feature rows (`EvoMu-w34`):
// - `feat:admin.gm-tournament`
// - `feat:serverops.session-changes`
// - `feat:removed.medical-clone-grades`
// Plus regression coverage for market-adjusted EIV and warable lookup filtering.

use crate::common;
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketHistoryDayRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

async fn login_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s.station_id = Some(JITA_STATION);
    ctx.advance_secs(12);
    s
}

async fn rpc(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await
}

fn find_notifs<'a>(notifs: &'a [PendingMachoNotification], event: &str) -> Vec<&'a PendingMachoNotification> {
    notifs.iter().filter(|n| n.event == event).collect()
}

fn session_change_pair(notif: &PendingMachoNotification, key: &str) -> Option<(PyRep, PyRep)> {
    let PyRep::Tuple(pair) = notif.args.get(2)?.dict_get(key)? else { return None };
    (pair.len() == 2).then(|| (pair[0].clone(), pair[1].clone()))
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_gm_tournament_match_bans_and_cleanup_grid() {
    // parity: feat:admin.gm-tournament
    eve_testkit::attest(&["feat:admin.gm-tournament"]);

    let ctx = ServerContext::new();
    let mut gm_sess = login_pilot(&ctx, 8901, "demo", "demo-secret", PILOT_A).await;
    let mut p2_sess = login_pilot(&ctx, 8902, "other", "other-secret", PILOT_B).await;

    // Negative 1: non-GM pilot cannot CreateMatch
    gm_sess.role = 0;
    let denied = rpc(
        &ctx,
        &mut gm_sess,
        "tourneyMgr",
        "CreateMatch",
        &[PyRep::Integer(1), PyRep::Integer(2401), PyRep::Integer(100), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0)],
    )
    .await
    .expect_err("non-GM CreateMatch must fail");
    assert!(!denied.code.is_empty());

    // 1. GM creates AT match with Red & Blue teams
    gm_sess.role = service_roles::ROLE_GMH;
    let create_rep = rpc(
        &ctx,
        &mut gm_sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(2401),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(PILOT_A),
                PyRep::Integer(PILOT_B),
            ]),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("GM CreateMatch");
    let PyRep::Tuple(create_tup) = create_rep else { panic!("expected CreateMatch 3-tuple") };
    assert_eq!(create_tup[0], PyRep::Integer(2401));
    let PyRep::List(beacons) = &create_tup[1] else { panic!("expected beacon list") };
    assert_eq!(beacons.len(), 2);
    let beacon_1 = beacons[0].as_i64().expect("beacon 1 id");

    // Negative 2: non-referee pilot cannot AddPlayer or CleanupGrid
    let p2_add_err = rpc(&ctx, &mut p2_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(1), PyRep::Integer(PILOT_B)])
        .await
        .expect_err("non-referee AddPlayer must fail");
    assert_eq!(p2_add_err.code, "AccessDenied");
    let p2_clean_err = rpc(&ctx, &mut p2_sess, "tourneyMgr", "CleanupGrid", &[PyRep::Integer(2401)])
        .await
        .expect_err("non-referee CleanupGrid must fail");
    assert_eq!(p2_clean_err.code, "AccessDenied");

    // 2. Referee adds Red and Blue pilots
    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(0), PyRep::Integer(PILOT_A)]).await.expect("AddPlayer Red"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(1), PyRep::Integer(PILOT_B)]).await.expect("AddPlayer Blue"),
        PyRep::Bool(true)
    );

    // 3. BanShip + FinalizeBans enforce ship hull bans during pregame checks
    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShip", &[PyRep::Integer(2401), PyRep::List(vec![PyRep::Integer(587)])]).await.expect("BanShip"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "FinalizeBans", &[PyRep::Integer(2401)]).await.expect("FinalizeBans"),
        PyRep::Bool(true)
    );
    let post_fin_ban = rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShip", &[PyRep::Integer(2401), PyRep::List(vec![PyRep::Integer(597)])])
        .await
        .expect_err("BanShip after FinalizeBans must fail");
    assert_eq!(post_fin_ban.code, "TourneyBansFinalized");
    let post_fin_bantype = rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(2401), PyRep::Integer(0), PyRep::Integer(597)])
        .await
        .expect_err("BanShipType after FinalizeBans must fail");
    assert_eq!(post_fin_bantype.code, "TourneyBansFinalized");
    let banned_check = rpc(&ctx, &mut gm_sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2401)])
        .await
        .expect_err("pilot in banned Rifter (587) must fail CompletePregameChecks");
    assert!(!banned_check.code.is_empty());

    // Switch both pilots to legal Punisher (597) hulls, clear recorded ban errors via OverrideError, and complete pregame
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, cid) in [PILOT_A, PILOT_B].into_iter().enumerate() {
            let sid = 930_000_100 + idx as i64;
            snap.items.insert(sid, make_item(sid, 597, cid, JITA_STATION, InventoryFlag::Hangar, 1, true));
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
            }
        }
    });
    for cid in [PILOT_A, PILOT_B] {
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "OverrideError", &[PyRep::Integer(2401), PyRep::Integer(cid), PyRep::String(String::new())])
            .await
            .expect("OverrideError");
    }
    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2401)]).await.expect("CompletePregameChecks"),
        PyRep::Bool(true)
    );
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    assert!(sys_handle.get_ball_snapshot(beacon_1).await.unwrap().is_some());
    rpc(&ctx, &mut gm_sess, "tourneyMgr", "StartCountdown", &[PyRep::Integer(2401)]).await.expect("StartCountdown");
    rpc(&ctx, &mut gm_sess, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(2401), PyRep::Integer(0)]).await.expect("MatchCompleted");
    assert!(sys_handle.get_ball_snapshot(beacon_1).await.unwrap().is_none());

    // 4. CleanupGrid removes arena-grid wrecks + cargo from both DB items and live ballpark while preserving ships and off-grid containers
    let (wreck_id, loot_id, ship_on_grid, offgrid_can_id) = (930_000_500_i64, 930_000_501_i64, 930_000_502_i64, 930_000_503_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_id, make_item(wreck_id, 26557, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
        snap.items.insert(loot_id, make_item(loot_id, 34, PILOT_A, wreck_id, InventoryFlag::Cargo, 1_000, false));
        snap.items.insert(ship_on_grid, make_item(ship_on_grid, 587, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
        snap.items.insert(offgrid_can_id, make_item(offgrid_can_id, 23, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
        snap.item_positions.insert(offgrid_can_id, [5_000_000.0, 0.0, 0.0]);
    });
    for (bid, tid, gid, x_pos) in [(wreck_id, 26557_u32, 186_u32, 1_500.0), (ship_on_grid, 587_u32, 25_u32, 1_500.0), (offgrid_can_id, 23_u32, 12_u32, 5_000_000.0)] {
        let _ = sys_handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: bid,
                    type_id: tid,
                    position: glam::DVec3::new(x_pos, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 25.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(tid),
                eve_destiny::SlimItemMeta {
                    group_id: gid,
                    category_id: if gid == 25 { 6 } else { 2 },
                    owner_id: PILOT_A,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    skin_id: None,
                    skin_material_set_id: None,
                    jump_target: None,
                },
            )
            .await;
    }
    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_some());

    assert_eq!(
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "CleanupGrid", &[PyRep::Integer(2401)]).await.expect("CleanupGrid"),
        PyRep::Bool(true)
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&wreck_id) && !s.items.contains_key(&loot_id) && s.items.contains_key(&ship_on_grid) && s.items.contains_key(&offgrid_can_id)));
    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(ship_on_grid).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(offgrid_can_id).await.unwrap().is_some());
}

#[tokio::test]
async fn test_session_attribute_changes_propagate_across_location_ship_corp_and_roles() {
    // parity: feat:serverops.session-changes
    eve_testkit::attest(&["feat:serverops.session-changes"]);

    let ctx = ServerContext::new();
    let mut sess_a = login_pilot(&ctx, 8903, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_pilot(&ctx, 8904, "other", "other-secret", PILOT_B).await;

    // 1. Location propagation: Undock -> in-space session + OnSessionChanged -> CmdDock -> docked session
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    rpc(&ctx, &mut sess_a, "ship", "Undock", &[]).await.expect("Undock");
    assert_eq!(sess_a.station_id, None);
    assert_eq!(sess_a.solar_system_id, Some(JITA_SYSTEM));
    let undock_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let undock_sc = find_notifs(&undock_notifs, "OnSessionChanged");
    assert!(undock_sc.iter().any(|n| session_change_pair(n, "stationid") == Some((PyRep::Integer(JITA_STATION), PyRep::None))));
    assert!(rpc(&ctx, &mut sess_a, "jumpCloneSvc", "InstallCloneInStation", &[]).await.is_err());

    ctx.advance_secs(12);
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    rpc(&ctx, &mut sess_a, "beyonce", "CmdDock", &[PyRep::Integer(JITA_STATION)]).await.expect("CmdDock");
    assert_eq!(sess_a.station_id, Some(JITA_STATION));
    let dock_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let dock_sc = find_notifs(&dock_notifs, "OnSessionChanged");
    assert!(dock_sc.iter().any(|n| session_change_pair(n, "stationid") == Some((PyRep::None, PyRep::Integer(JITA_STATION)))));

    // 2. Ship propagation: ship.Board updates session.ship_id, emits OnSessionChanged("shipid"), and syncs character sheet
    ctx.advance_secs(12);
    let old_ship_a = sess_a.ship_id.expect("initial ship_id");
    let slasher_id = 931_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(slasher_id, make_item(slasher_id, 585, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    rpc(&ctx, &mut sess_a, "ship", "Board", &[PyRep::Integer(slasher_id)]).await.expect("ship.Board");
    assert_eq!(sess_a.ship_id, Some(slasher_id));
    let board_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let board_sc = find_notifs(&board_notifs, "OnSessionChanged");
    assert!(board_sc.iter().any(|n| session_change_pair(n, "shipid") == Some((PyRep::Integer(old_ship_a), PyRep::Integer(slasher_id)))));
    let info_a = rpc(&ctx, &mut sess_a, "charMgr", "GetCharacterInfo", &[]).await.expect("GetCharacterInfo");
    assert_eq!(info_a.dict_get("shipID"), Some(&PyRep::Integer(slasher_id)));
    assert_eq!(info_a.dict_get("shipTypeID"), Some(&PyRep::Integer(585)));

    // 3. Corp propagation: PILOT_B joins CEO PILOT_A's player corporation -> OnSessionChanged("corpid") + auto-sync
    let player_corp_id = 98_500_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: player_corp_id,
            name: "Session Propagation Corp".into(),
            ticker: "SPROP".into(),
            ceo_id: PILOT_A,
            hq_station_id: Some(JITA_STATION),
            ..Default::default()
        };
        corp.members.insert(PILOT_A);
        corp.member_roles.insert(PILOT_A, u64::MAX);
        snap.corporations.insert(player_corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = player_corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.station_id = Some(JITA_STATION);
        }
    });
    sess_a.corporation_id = Some(player_corp_id);
    sess_a.corp_role = u64::MAX;
    ctx.advance_secs(12);

    let old_corp_b = sess_b.corporation_id.expect("initial NPC corp");
    let app_rep = rpc(
        &ctx,
        &mut sess_b,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(player_corp_id), PyRep::String("Applying".into())],
    )
    .await
    .expect("InsertApplication");
    let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("applicationID");
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("UpdateApplicationOffer Accepted");

    let join_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let join_sc = find_notifs(&join_notifs, "OnSessionChanged");
    assert!(join_sc.iter().any(|n| session_change_pair(n, "corpid") == Some((PyRep::Integer(old_corp_b), PyRep::Integer(player_corp_id)))));

    // 4. Roles propagation: unprivileged member denied -> CEO UpdateMember grants role -> OnSessionChanged("corprole") + live sync -> revoke blocks again
    let corp_fit = PyRep::keyval([
        ("name", PyRep::String("Doctrine Punisher".into())),
        ("shipTypeID", PyRep::Integer(597)),
        ("description", PyRep::String("Role-gated corp fit".into())),
        ("fitData", PyRep::List(vec![])),
    ]);
    assert!(
        rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit.clone()])
            .await
            .is_err()
    );
    assert_eq!(sess_b.corporation_id, Some(player_corp_id));

    let fitting_mgr_role: u64 = 1u64 << 41;
    let hq_role: u64 = 1u64 << 7;
    let delayed_hangar_role: u64 = 1u64 << 13; // Hangar Take 1 (24h delayed grant)
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(PILOT_B)),
            ("roles", PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64)),
            ("rolesAtHQ", PyRep::Integer(hq_role as i64)),
        ])],
    )
    .await
    .expect("UpdateMember grant");

    let grant_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let grant_sc = find_notifs(&grant_notifs, "OnSessionChanged");
    let role_notif = grant_sc
        .iter()
        .find(|n| session_change_pair(n, "corprole") == Some((PyRep::Integer(0), PyRep::Integer(fitting_mgr_role as i64))))
        .expect("OnSessionChanged with immediate corprole");
    assert_eq!(role_notif.id_type, "charid", "OnSessionChanged must address charid");
    assert_eq!(role_notif.args[1].dict_get("charid"), Some(&PyRep::Integer(PILOT_B)));
    assert_eq!(role_notif.args[1].dict_get("corpid"), Some(&PyRep::Integer(player_corp_id)));
    assert_eq!(role_notif.args[1].dict_get("corprole"), Some(&PyRep::Integer(fitting_mgr_role as i64)));
    assert_eq!(role_notif.args[1].dict_get("rolesAtHQ"), Some(&PyRep::Integer(hq_role as i64)));
    assert_eq!(session_change_pair(role_notif, "rolesAtHQ"), Some((PyRep::Integer(0), PyRep::Integer(hq_role as i64))));

    let saved_fid = rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit.clone()])
        .await
        .expect("member live session must pick up granted role")
        .as_i64()
        .expect("saved corp fitting id");
    assert!(saved_fid > 0);
    assert_eq!(sess_b.corp_role, fitting_mgr_role);

    // Non-Director Fitting Manager can also call corpFittingMgr.UpdateNameAndDescription
    let renamed = rpc(
        &ctx,
        &mut sess_b,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(saved_fid),
            PyRep::Integer(player_corp_id),
            PyRep::String("Renamed Doctrine Punisher".into()),
            PyRep::String("Updated by Fitting Manager".into()),
        ],
    )
    .await
    .expect("Fitting Manager UpdateNameAndDescription");
    assert_eq!(renamed.dict_get("name"), Some(&PyRep::String("Renamed Doctrine Punisher".into())));

    // Advance 24h so pending delayed hangar role matures and emits OnSessionChanged
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_secs(86_405);
    let _ = rpc(&ctx, &mut sess_b, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_B)]).await.expect("GetMember triggers pending grant promotion");
    let matured_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let matured_sc = find_notifs(&matured_notifs, "OnSessionChanged");
    assert!(
        matured_sc.iter().any(|n| n.id_type == "charid"
            && session_change_pair(n, "corprole") == Some((PyRep::Integer(fitting_mgr_role as i64), PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64)))),
        "Maturing 24h delayed role grant must emit OnSessionChanged with updated corprole"
    );

    let _ = ctx.drain_notifications_for_char(PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::keyval([("characterID", PyRep::Integer(PILOT_B)), ("roles", PyRep::Integer(0)), ("rolesAtHQ", PyRep::Integer(0))])],
    )
    .await
    .expect("UpdateMember revoke");
    let revoke_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let revoke_sc = find_notifs(&revoke_notifs, "OnSessionChanged");
    assert!(revoke_sc.iter().any(|n| n.id_type == "charid"
        && session_change_pair(n, "corprole") == Some((PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64), PyRep::Integer(0)))
        && session_change_pair(n, "rolesAtHQ") == Some((PyRep::Integer(hq_role as i64), PyRep::Integer(0)))));
    assert!(
        rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit])
            .await
            .is_err()
    );
    assert_eq!(sess_b.corp_role, 0);
}

#[tokio::test]
async fn test_medical_clone_grades_removed_free_respawn_zero_sp_loss() {
    // parity: feat:removed.medical-clone-grades
    eve_testkit::attest(&["feat:removed.medical-clone-grades"]);

    let ctx = ServerContext::new();
    let mut sess = login_pilot(&ctx, 8905, "demo", "demo-secret", PILOT_A).await;

    // Seed a 52,000,000 SP veteran pilot (far above pre-Rhea 900,000 SP basic medical clone cap)
    let initial_balance = IskCents(25_000_000_00);
    let initial_sp = 52_000_000.0_f64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.balance = initial_balance;
            ch.skill_points = initial_sp;
            ch.home_station_id = JITA_STATION;
            ch.station_id = Some(JITA_STATION);
        }
        let smap = snap.skills.entry(PILOT_A).or_default();
        smap.insert(3300, TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
        smap.insert(3332, TrainedSkillRecord { type_id: 3332, level: 5, skill_points: 512_000.0 });
    });

    // V24 jumpCloneSvc.GetCloneState exposes jump clones, implants, and cooldown — no medical clone grade/SP cap
    let clone_state = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState");
    assert!(clone_state.dict_get("clones").is_some());
    assert!(clone_state.dict_get("cloneGrade").is_none());

    // Negative: home_station.self_destruct_clone while boarded in a non-capsule ship fails
    let non_pod_err = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
        .await
        .expect_err("self_destruct_clone requires capsule");
    assert!(!non_pod_err.code.is_empty());

    // Leave ship into Capsule (670) and trigger medical clone activation at home station
    ctx.advance_secs(12);
    let capsule_id = rpc(&ctx, &mut sess, "ship", "LeaveShip", &[])
        .await
        .expect("LeaveShip")
        .as_i64()
        .expect("capsule id");
    assert_eq!(sess.ship_id, Some(capsule_id));
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let respawn_station = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
        .await
        .expect("medical clone activation")
        .as_i64()
        .expect("home station id");
    assert_eq!(respawn_station, JITA_STATION);
    assert_eq!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnCloneDestruction").len(), 1);

    // Verify 0 ISK fee and 100% SP + level-V skill preservation
    let sheet = rpc(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[])
        .await
        .expect("GetCharacterInfo after medical clone respawn");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&PILOT_A).map(|c| c.balance)),
        Some(initial_balance),
        "Medical clone respawn must be completely free (0 ISK charged)"
    );
    assert_eq!(
        sheet.dict_get("skillPoints").and_then(|v| v.as_f64()),
        Some(initial_sp),
        "Medical clone respawn must preserve 100% of character SP without pre-Rhea grade loss"
    );
    let (lvl_3300, lvl_3332) = ctx.db.with_snapshot(|s| {
        let smap = s.skills.get(&PILOT_A).expect("skills map");
        (smap[&3300].level, smap[&3332].level)
    });
    assert_eq!((lvl_3300, lvl_3332), (5, 5));
}

#[tokio::test]
async fn test_eiv_market_adjusted_prices_and_warable_lookup_regressions() {
    let ctx = ServerContext::new();
    let mut sess = login_pilot(&ctx, 8906, "demo", "demo-secret", PILOT_A).await;

    let bp = ctx.sde.blueprints.get(&687).cloned().expect("Rifter BP");
    let baseline_eiv = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::industry_facilities_bps::compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), &bp, 1)
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.insert(
            (10_000_002, 34),
            (1..=10)
                .map(|d| MarketHistoryDayRecord {
                    date: format!("2026-10-{d:02}"),
                    average: 25.0,
                    highest: 26.0,
                    lowest: 24.0,
                    order_count: 100,
                    volume: 1_000_000,
                    open_price: None,
                    close_price: None,
                })
                .collect(),
        );
    });
    let elevated_eiv = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::industry_facilities_bps::compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), &bp, 1)
    });
    assert!(elevated_eiv > baseline_eiv);

    sess.corporation_id = Some(98_000_001);
    sess.alliance_id = Some(99_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_600_001,
            CorporationRecord {
                corporation_id: 98_600_001,
                name: "Standalone WarCitadel Corp".into(),
                ticker: "SWCC".into(),
                ceo_id: 90_000_050,
                alliance_id: None,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_600_002,
            CorporationRecord {
                corporation_id: 98_600_002,
                name: "Allied WarCitadel Member Corp".into(),
                ticker: "AWMC".into(),
                ceo_id: 90_000_051,
                alliance_id: Some(99_600_002),
                ..Default::default()
            },
        );
        for (sid, owner) in [(1_000_600_001_001_i64, 98_600_001_i64), (1_000_600_002_001_i64, 98_600_002_i64)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: owner,
                    solar_system_id: JITA_SYSTEM,
                    name: "WarCitadel Astrahus".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 100.0,
                    armor_hp: 100.0,
                    hull_hp: 100.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        snap.alliances.insert(
            99_600_002,
            AllianceRecord {
                alliance_id: 99_600_002,
                name: "WarCitadel Target Alliance".into(),
                short_name: "WCTA".into(),
                executor_corp_id: 98_600_002,
                member_corp_ids: [98_600_002].into_iter().collect(),
            },
        );
    });

    let PyRep::List(rows) = rpc(
        &ctx,
        &mut sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("WarCitadel".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances") else {
        panic!("expected List");
    };
    let returned_ids: Vec<i64> = rows.iter().filter_map(|r| r.dict_get("ownerID").and_then(|v| v.as_i64())).collect();
    assert!(returned_ids.contains(&98_600_001));
    assert!(returned_ids.contains(&99_600_002));
    assert!(!returned_ids.contains(&98_600_002));
}

#[tokio::test]
async fn test_industry_container_division_and_reactor_gates() {
    let ctx = ServerContext::new();
    let mut sess = login_pilot(&ctx, 8911, "demo", "demo-secret", PILOT_A).await;
    let corp_id = 98_700_001_i64;
    let fm_sag1_only = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id, name: "Gate Corp".into(), ticker: "GATE".into(),
            ceo_id: PILOT_B, hq_station_id: Some(JITA_STATION), master_wallet: IskCents(100_000_000_00), ..Default::default()
        };
        corp.members.insert(PILOT_A);
        corp.member_roles.insert(PILOT_A, fm_sag1_only);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.corporation_id = corp_id; ch.balance = IskCents(100_000_000_00); }
        snap.items.insert(931_000_001, make_item(931_000_001, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        snap.items.insert(931_000_002, make_item(931_000_002, 3293, PILOT_A, 60003761, InventoryFlag::Hangar, 1, true));
        snap.items.insert(931_000_003, make_item(931_000_003, 687, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, 1, true));
        snap.structures.insert(931_000_010, crate::action_plan_mining::make_structure(931_000_010, corp_id, 35835, 30002813, "Lowsec Athanor", [0.0, 0.0, 0.0]));
        snap.items.insert(931_000_011, make_item(931_000_011, 35891, corp_id, 931_000_010, InventoryFlag::ServiceSlot0, 1, true));
        snap.items.insert(931_000_012, make_item(931_000_012, 90274, PILOT_A, 931_000_010, InventoryFlag::Hangar, 1, true));
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = fm_sag1_only;

    let remote_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(931_000_001)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)), ("inputContainerID", PyRep::Integer(931_000_002)),
    ])]).await.expect_err("remote inputContainerID must fail");
    assert_eq!(remote_err.code, "InvalidInputContainer");

    let div_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(931_000_003)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
        ("useCorp", PyRep::Bool(true)), ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG2 as i64)),
    ])]).await.expect_err("unauthorized CorpSAG2 must fail");
    assert_eq!(div_err.code, "CrpAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None; ch.structure_id = Some(931_000_010); ch.solar_system_id = 30002813;
        }
    });
    sess.station_id = None; sess.structure_id = Some(931_000_010); sess.solar_system_id = Some(30002813);
    let react_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(931_000_012)), ("facilityID", PyRep::Integer(931_000_010)),
        ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("35891 Standup Research Lab I must be rejected for composite reaction");
    assert_eq!(react_err.code, "CompositeReactorRequired");
}

}
