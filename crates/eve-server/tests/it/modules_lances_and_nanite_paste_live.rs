//! Live test suite for Disruptive Lance, Titan Lance Doomsday, and Nanite Repair Paste:
//! - `feat:modules.nanite-repair-paste-in-space`
//! - `feat:modules.disruptive-lance`
//! - `feat:modules.lance-doomsday-directional-geometry`
//!
//! Fully authentic live integration suite using real player RPCs, GM setup commands,
//! and destiny physics simulation ticks with zero snapshot mutations.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const NULL_SYS: u32 = 30_004_759; // 1-SMEB in Syndicate (null-sec, security -0.2)

fn extract_attr_from_info(info_rep: &PyRep, attr_id: i64) -> f64 {
    let Some(attrs) = info_rep.dict_get("attributes") else { return 0.0 };
    if let PyRep::Dict(entries) = attrs {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
}

/// Provisions a live pilot, fits modules and loads cargo while docked, trains required skills,
/// and flies into the destination space system via real client RPCs.
async fn spawn_fitted_pilot_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    fitted_modules: &[(u32, i64)],
    cargo_items: &[(u32, i32, i64)], // (type_id, qty, target_flag)
    extra_skills: &[(u32, u8)],
) -> (LivePilot, Vec<i64>, Vec<i64>) {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = eve_machonet::SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await;
    let ship_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem itemID");

    let mut skill_set: Vec<u32> = ctx.sde.types.get(&ship_type_id).map(|t| {
        [182_u16, 183, 184, 1285, 1289, 1290].iter().filter_map(|a| t.default_attributes.get(a)).map(|v| v.round() as u32).filter(|&s| s > 0).collect()
    }).unwrap_or_default();
    for &(mod_tid, _) in fitted_modules {
        if let Some(mt) = ctx.sde.types.get(&mod_tid) {
            for &attr in &[182_u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&req) = mt.default_attributes.get(&attr).filter(|v| **v > 0.0) {
                    skill_set.push(req as u32);
                }
            }
        }
    }
    for sid in skill_set {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for &(sid, lvl) in extra_skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} {lvl}")).await;
    }

    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None).await.expect("ship.Board");
    ctx.advance_secs(31);

    // Fit modules while docked at station
    let mut fitted_ids = Vec::new();
    for &(mod_tid, flag) in fitted_modules {
        let m_created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {mod_tid} 1")).await;
        let m_id = m_created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem mod itemID");
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(m_id), PyRep::Integer(ship_id), PyRep::Integer(flag)], &kw, None)
            .await
            .unwrap_or_else(|e| panic!("invbroker.Add mod {mod_tid} -> flag {flag} failed: {e:?}"));
        fitted_ids.push(m_id);
    }

    // Place initial cargo / fuel while docked
    let mut cargo_ids = Vec::new();
    for &(c_tid, qty, flag) in cargo_items {
        let c_created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {c_tid} {qty}")).await;
        let c_id = c_created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem cargo itemID");
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(c_id), PyRep::Integer(ship_id), PyRep::Integer(flag)], &kw, None)
            .await
            .unwrap_or_else(|e| panic!("invbroker.Add cargo {c_tid} -> flag {flag} failed: {e:?}"));
        cargo_ids.push(c_id);
    }

    // Undock and jump to destination space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("stationSvc.Undock");
    common::slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);

    let bind = ctx.dispatch_rpc(&mut sess, "beyonce", None, "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None], &kw, None)
        .await
        .expect("beyonce.MachoBindObject");
    let PyRep::Tuple(bound) = bind else { panic!("bind tuple") };
    let beyonce = bound[0].as_i64().expect("beyonce bound id") as u64;
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);

    (LivePilot { sess, char_id, ship_id, beyonce }, fitted_ids, cargo_ids)
}

#[tokio::test]
async fn test_live_nanite_repair_paste_in_space() {
    // parity: feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.set_mechanic_rng_seed(Some(2026));
    let kw = HashMap::new();

    // 1. Megathron (641) flown in space by live pilot with Small Focused Pulse Laser II (3041) fitted in HiSlot0 (flag 27)
    let (mut pilot, fitted, cargo) = spawn_fitted_pilot_in_space(
        &ctx,
        101,
        91_300_001,
        641,
        NULL_SYS,
        &[(3041, 27)], // Small Focused Pulse Laser II
        &[(12565, 1, 5)], // Conflagration S crystal in cargo (flag 5)
        &[
            (28164, 5), // Thermodynamics V
            (28879, 5), // Nanite Operation V
            (28880, 5), // Nanite Interfacing V
            (3336, 5),  // Large Hybrid Turret V
            (3423, 5),  // Energy Management V
        ],
    )
    .await;
    let laser = fitted[0];
    let crystal = cargo[0];
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    // Load crystal into laser
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(pilot.ship_id),
            PyRep::List(vec![PyRep::Integer(laser)]),
            PyRep::List(vec![PyRep::Integer(crystal)]),
            PyRep::Integer(pilot.ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo crystal into laser");

    // Spawn dummy target drone at 2 km and lock it
    let target = common::gm_pilot_in_space(&ctx, 102, 91_300_002, 624, NULL_SYS).await; // Stabber
    let origin = sys.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap().1.position;
    sys.set_ball_position(target.ship_id, origin + DVec3::new(2_000.0, 0.0, 0.0)).await;

    ctx.dispatch_rpc(
        &mut pilot.sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(target.ship_id)],
        &kw,
        None,
    )
    .await
    .expect("AddTarget");

    // Step simulation ticks to finish target lock
    let mut locked = false;
    for _ in 0..40 {
        let _ = sys.step_manual(1.0).await;
        if sys
            .get_target_locks(pilot.ship_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|l| l.target_id == target.ship_id && l.locked)
        {
            locked = true;
            break;
        }
    }
    assert!(locked, "Target lock must complete");

    // Overload laser and activate it against target
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "dogmaIM",
        None,
        "Overload",
        &[
            PyRep::Integer(laser),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(target.ship_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Overload laser");

    // Set realistic heat damage on laser via live GM slash command
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/setattr {laser} 3 15.0")).await;
    let info = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(laser)], &kw, None)
        .await
        .unwrap();
    let initial_mod_dmg = extract_attr_from_info(&info, 3);
    assert_eq!(initial_mod_dmg, 15.0, "Laser must have 15 HP heat damage before repair");

    // 2. Active / overloaded module cannot be repaired with Nanite Repair Paste until deactivated
    let active_rep_err = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(laser)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(active_rep_err.code, "CannotRepairWhileOverloaded");

    // Deactivate module
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(laser), PyRep::String("targetAttack".into())],
        &kw,
        None,
    )
    .await
    .expect("Deactivate laser");
    let _ = sys.step_manual(3.0).await;

    // 3. Attempting repair with 0 paste in cargo is rejected
    let no_paste_err = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(laser)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(no_paste_err.code, "NotEnoughNanitePaste");

    // 4. Stock Nanite Repair Paste (28668) into cargo in space
    let paste_stack = common::stock_ship_bay(&ctx, &mut pilot, 28668, 50, 5).await;
    let paste_cargo_before = ctx.db.with_snapshot(|s| s.items.get(&paste_stack).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(paste_cargo_before, 50);

    // 5. InitiateModuleRepair consumes paste from cargo and starts repair
    let rep_start = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(laser)], &kw, None)
        .await
        .expect("InitiateModuleRepair");
    let total_paste = rep_start.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
    let paste_consumed_init = rep_start.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let repair_rate = rep_start.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert!(total_paste >= 1, "Must require paste: {total_paste}");
    assert_eq!(paste_consumed_init, 1, "1 paste deducted at start of repair");
    assert!(repair_rate > 0.0, "Repair rate must be positive: {repair_rate}");

    // 6. Step 0.5s ticks to progress repair, then test StopModuleRepair
    let _ = sys.step_manual(0.5).await;
    let stop_rep = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(laser)], &kw, None)
        .await
        .expect("StopModuleRepair");
    assert_eq!(stop_rep.dict_get("stopped"), Some(&PyRep::Bool(true)));

    // 7. Restart repair and step ticks until fully completed
    let _ = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(laser)], &kw, None)
        .await
        .expect("Restart InitiateModuleRepair");

    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }

    let final_info = ctx
        .dispatch_rpc(&mut pilot.sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(laser)], &kw, None)
        .await
        .unwrap();
    let final_dmg = extract_attr_from_info(&final_info, 3);
    assert_eq!(final_dmg, 0.0, "Nanite Repair Paste must completely repair heat damage on fitted module");

    let paste_cargo_after = ctx.db.with_snapshot(|s| s.items.get(&paste_stack).map(|i| i.quantity).unwrap_or(0));
    assert!(paste_cargo_after < 50, "Nanite repair paste was consumed from ship cargo ({paste_cargo_after} < 50)");
}

#[tokio::test]
async fn test_live_disruptive_lance() {
    // parity: feat:modules.disruptive-lance
    eve_testkit::attest(&["feat:modules.disruptive-lance"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Lancer Dreadnought: Bane (77283, group 4594) with Siege II, Disruptive Lance, and Cloak fitted while docked
    let (mut lancer, fitted, cargo) = spawn_fitted_pilot_in_space(
        &ctx,
        201,
        91_300_011,
        77283,
        NULL_SYS,
        &[
            (4292, 27),  // Siege Module II (HiSlot0)
            (77399, 28), // Disruptive Lance (HiSlot1)
            (11370, 29), // Cloaking Device (HiSlot2)
        ],
        &[
            (16275, 250, 5),     // Strontium Clathrates in Cargo (flag 5, 750 m3)
            (16274, 30_000, 133), // Heavy Water / Isotopes in SpecializedFuelBay (flag 133)
        ],
        &[
            (77739, 5), // Disruptive Lance Operation
            (77738, 5), // Lancer Dreadnoughts
            (22043, 5), // Tactical Weapon Reconfiguration
            (3423, 5),  // Energy Management
        ],
    )
    .await;
    let siege = fitted[0];
    let lance = fitted[1];
    let cloak = fitted[2];
    let iso_stack = cargo[1];
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    // 2. Cloak activation while Lance is fitted must fail
    let cloak_err = ctx
        .dispatch_rpc(&mut lancer.sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(cloak_err.code, "CannotCloakWithLanceFitted");

    // Spawn victim ship in corridor at (40km, 0, 0)
    let mut victim = common::gm_pilot_in_space(&ctx, 202, 91_300_012, 19724, NULL_SYS).await; // Revelation
    let origin = sys.get_ball_snapshot(lancer.ship_id).await.unwrap().unwrap().1.position;
    sys.set_ball_position(victim.ship_id, origin + DVec3::new(40_000.0, 0.0, 0.0)).await;

    // Spawn bystander ship outside corridor at (40km, 15km, 0)
    let bystander = common::gm_pilot_in_space(&ctx, 203, 91_300_013, 624, NULL_SYS).await; // Stabber
    sys.set_ball_position(bystander.ship_id, origin + DVec3::new(40_000.0, 15_000.0, 0.0)).await;

    // 3. Lance activation rejected before Siege is active
    let no_siege_err = ctx
        .dispatch_rpc(
            &mut lancer.sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(lance), PyRep::String("default".into()), PyRep::Integer(victim.ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(no_siege_err.code, "SiegeRequiredForLance");

    // Activate Siege Module
    ctx.dispatch_rpc(&mut lancer.sess, "dogmaIM", None, "Activate", &[PyRep::Integer(siege)], &kw, None)
        .await
        .expect("Activate Siege");

    let v_shield0 = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().shield_hp;
    let b_shield0 = sys.get_combat_state(bystander.ship_id).await.unwrap().unwrap().shield_hp;

    // 4. Fire Disruptive Lance at victim
    let lance_res = ctx
        .dispatch_rpc(
            &mut lancer.sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(lance), PyRep::String("default".into()), PyRep::Integer(victim.ship_id)],
            &kw,
            None,
        )
        .await
        .expect("Fire Disruptive Lance");

    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(lance_res.dict_get("casterLockSeconds").and_then(|v| v.as_i64()), Some(300), "Caster locked for 300s");
    assert_eq!(lance_res.dict_get("victimDebuffSeconds").and_then(|v| v.as_i64()), Some(60));
    assert_eq!(lance_res.dict_get("remoteRepairImpedance").and_then(|v| v.as_f64()), Some(0.50), "50% repair impedance");
    assert_eq!(lance_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(20_000), "Consumed 20,000 isotopes");

    let rem_iso = ctx.db.with_snapshot(|s| s.items.get(&iso_stack).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_iso, 10_000, "30,000 - 20,000 = 10,000 isotopes remain in Fuel Bay");

    // 5. Victim took damage, warp disruption, and tether break
    let v_cs = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap();
    assert!(v_cs.shield_hp < v_shield0, "Victim in beam took lance damage ({} < {v_shield0})", v_cs.shield_hp);
    assert_eq!(v_cs.warp_scramble_strength, 100, "Victim warp disrupted");
    assert!(!v_cs.is_tethered, "Victim tether broken");

    // Bystander outside beam took 0 damage
    let b_cs = sys.get_combat_state(bystander.ship_id).await.unwrap().unwrap();
    assert_eq!(b_cs.shield_hp, b_shield0, "Bystander outside beam corridor must take zero damage");

    // 6. Victim cannot warp during 60s debuff
    let warp_err = ctx
        .dispatch_rpc(
            &mut victim.sess,
            "beyonce",
            Some(victim.beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(40_000_001)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(warp_err.code, "DisruptiveLanceDebuffActive");
}

#[tokio::test]
async fn test_live_lance_doomsday_directional_geometry() {
    // parity: feat:modules.lance-doomsday-directional-geometry
    eve_testkit::attest(&["feat:modules.lance-doomsday-directional-geometry"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Titan pilot in Avatar (11567, group 30) with Titan Lance Doomsday (40631) fitted while docked
    let (mut titan, fitted, cargo) = spawn_fitted_pilot_in_space(
        &ctx,
        301,
        91_300_021,
        11567,
        NULL_SYS,
        &[(40631, 27)],       // Titan Lance Doomsday in HiSlot0
        &[(16274, 100_000, 5)], // Isotopes in cargo
        &[
            (3347, 5),  // Amarr Titan
            (21803, 5), // Doomsday Operation
            (3423, 5),  // Energy Management
        ],
    )
    .await;
    let lance_mod = fitted[0];
    let iso_stack = cargo[0];
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let titan_pos = sys.get_ball_snapshot(titan.ship_id).await.unwrap().unwrap().1.position;

    // Target 1: inside beam corridor at (100km, 0, 0) along +X
    let beam_target = common::gm_pilot_in_space(&ctx, 302, 91_300_022, 23757, NULL_SYS).await; // Archon
    sys.set_ball_position(beam_target.ship_id, titan_pos + DVec3::new(100_000.0, 0.0, 0.0)).await;

    // Target 2: beyond beam length at (350km, 0, 0) along +X (> 200km range)
    let far_target = common::gm_pilot_in_space(&ctx, 303, 91_300_023, 641, NULL_SYS).await;
    sys.set_ball_position(far_target.ship_id, titan_pos + DVec3::new(350_000.0, 0.0, 0.0)).await;

    // Target 3: off-axis target at (100km, 20km, 0) (> 2.5km beam radius)
    let off_target = common::gm_pilot_in_space(&ctx, 304, 91_300_024, 641, NULL_SYS).await;
    sys.set_ball_position(off_target.ship_id, titan_pos + DVec3::new(100_000.0, 20_000.0, 0.0)).await;

    let cs_beam_init = sys.get_combat_state(beam_target.ship_id).await.unwrap().unwrap();
    let cs_far_init = sys.get_combat_state(far_target.ship_id).await.unwrap().unwrap();
    let cs_off_init = sys.get_combat_state(off_target.ship_id).await.unwrap().unwrap();

    // 2. Fire Titan Lance Doomsday along +X direction (segment from titan_pos to titan_pos + 200km)
    let origin_arg = PyRep::Tuple(vec![PyRep::Real(titan_pos.x), PyRep::Real(titan_pos.y), PyRep::Real(titan_pos.z)]);
    let endpoint_arg = PyRep::Tuple(vec![PyRep::Real(titan_pos.x + 200_000.0), PyRep::Real(titan_pos.y), PyRep::Real(titan_pos.z)]);

    let slash_res = ctx
        .dispatch_rpc(
            &mut titan.sess,
            "superWeaponMgr",
            None,
            "ActivateSlashModule",
            &[PyRep::Integer(lance_mod), origin_arg, endpoint_arg],
            &kw,
            None,
        )
        .await
        .expect("ActivateSlashModule");

    assert_eq!(slash_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
    assert_eq!(slash_res.dict_get("jumpBlockSeconds"), Some(&PyRep::Real(300.0)));
    assert_eq!(slash_res.dict_get("fuelConsumed"), Some(&PyRep::Integer(50_000)));

    let rem_iso = ctx.db.with_snapshot(|s| s.items.get(&iso_stack).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_iso, 50_000, "100,000 - 50,000 = 50,000 isotopes remain in Titan cargo");

    // 3. During 15s warmup: Titan is immobilized and cannot steer or warp
    assert!(
        ctx.dispatch_rpc(
            &mut titan.sess,
            "beyonce",
            Some(titan.beyonce),
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .is_err(),
        "Titan must be immobilized during lance warmup"
    );
    assert!(
        ctx.dispatch_rpc(
            &mut titan.sess,
            "beyonce",
            Some(titan.beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(40_000_001), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .is_err(),
        "Titan cannot warp during lance warmup"
    );

    // Step 12s (< 15s warmup): no damage dealt yet
    for _ in 0..12 {
        let _ = sys.step_manual(1.0).await;
    }
    let cs_beam_mid = sys.get_combat_state(beam_target.ship_id).await.unwrap().unwrap();
    assert_eq!(cs_beam_mid.shield_hp, cs_beam_init.shield_hp, "No damage before 15s warmup finishes");

    // Step remaining ticks past 15s boundary
    for _ in 0..10 {
        let _ = sys.step_manual(1.0).await;
    }

    // 4. Directional geometry verification:
    // Ship in beam path takes massive damage (or is obliterated), while ship beyond 200km and off-axis ship take zero damage
    let cs_far_after = sys.get_combat_state(far_target.ship_id).await.unwrap().unwrap();
    let cs_off_after = sys.get_combat_state(off_target.ship_id).await.unwrap().unwrap();
    let beam_damaged_or_destroyed = sys
        .get_combat_state(beam_target.ship_id)
        .await
        .unwrap()
        .map(|cs| cs.shield_hp < cs_beam_init.shield_hp)
        .unwrap_or(true);

    assert!(beam_damaged_or_destroyed, "Ship in beam cone takes massive damage or is destroyed");
    assert_eq!(cs_far_after.shield_hp, cs_far_init.shield_hp, "Ship beyond range takes 0 damage");
    assert_eq!(cs_off_after.shield_hp, cs_off_init.shield_hp, "Ship off-axis takes 0 damage");

    // Titan caster cannot warp during 300s lock
    assert!(
        ctx.dispatch_rpc(
            &mut titan.sess,
            "beyonce",
            Some(titan.beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(40_000_001), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .is_err(),
        "Titan cannot warp during 300s caster lock"
    );
}
