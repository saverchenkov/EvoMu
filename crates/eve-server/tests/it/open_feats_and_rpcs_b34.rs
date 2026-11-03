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
