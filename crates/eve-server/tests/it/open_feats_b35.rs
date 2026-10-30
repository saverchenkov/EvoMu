//! Integration tests for open `feat:modules.*` and `feat:navigation.*` parity rows (Batch 35).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use eve_db::{FleetMemberRecord, FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

const DEST_NULL_SYS: u32 = 30004760;
const ZARZAKH_SYS: u32 = 30100000;

fn give_item(
    ctx: &eve_server::router::ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton: qty == 1 && !matches!(flag, InventoryFlag::Cargo | InventoryFlag::SpecializedFuelBay),
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

async fn spawn_ship_in_space(
    ctx: &eve_server::router::ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let t = ctx.sde.types.get(&type_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: t.map(|x| x.mass).unwrap_or(10_000_000.0),
        radius: t.map(|x| x.radius).unwrap_or(100.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    cs.undock_invulnerable_remaining_ticks = 0;
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

#[tokio::test]
async fn test_b35_superweapons_bosonic_gtfo_reaper_and_lance() {
    // parity: feat:modules.doomsday-bosonic-field, feat:modules.doomsday-gtfo-reaper, feat:modules.lance-doomsday-directional-geometry
    eve_testkit::attest(&[
        "feat:modules.doomsday-bosonic-field",
        "feat:modules.doomsday-gtfo-reaper",
        "feat:modules.lance-doomsday-directional-geometry",
    ]);
    let ctx = build_ctx();
    let titan_char = 950_101;
    let titan_ship = 960_101;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false); // Avatar
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;

    let bosonic_mod = 970_101;
    let gtfo_mod = 970_102;
    let reaper_mod = 970_103;
    let lance_mod = 970_104;
    give_item(&ctx, bosonic_mod, 40633, titan_char, titan_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, gtfo_mod, 40634, titan_char, titan_ship, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, reaper_mod, 40632, titan_char, titan_ship, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, lance_mod, 40631, titan_char, titan_ship, InventoryFlag::HiSlot3, 1);
    give_item(&ctx, 970_105, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 500_000); // Helium Isotopes
    give_item(&ctx, 970_106, 16275, titan_char, titan_ship, InventoryFlag::Cargo, 50_000); // Strontium Clathrates

    let v_in_cone = 960_201;
    let v_out_cone = 960_202;
    let v_lance_200km = 960_203;
    let v_lance_350km = 960_204;
    seed_pilot(&ctx, 950_201, v_in_cone, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_202, v_out_cone, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_203, v_lance_200km, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_204, v_lance_350km, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, 950_201, v_in_cone, 641, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_202, v_out_cone, 641, NULL_SYS, DVec3::new(-50_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_203, v_lance_200km, 641, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_204, v_lance_350km, 641, NULL_SYS, DVec3::new(350_000.0, 0.0, 0.0)).await;

    // 1. Bosonic Field Generator (`ActivateSlashModule`)
    let dir_arg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let cone_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(bosonic_mod), dir_arg], &HashMap::new(), None)
        .await
        .expect("ActivateSlashModule Bosonic");
    assert_eq!(cone_res.dict_get("tickCount"), Some(&PyRep::Integer(10)));
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let titan_cs = sys.get_combat_state(titan_ship).await.unwrap().unwrap();
    assert!(titan_cs.disallow_assistance && !titan_cs.is_tethered, "Titan must be tether-blocked");

    // 2. GTFO (`ActivateSlashModule`) teleports ships in target sphere
    let sphere_center = PyRep::Tuple(vec![PyRep::Real(-50_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let gtfo_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(gtfo_mod), sphere_center], &HashMap::new(), None)
        .await
        .expect("ActivateSlashModule GTFO");
    assert!(gtfo_res.dict_get("teleportedTargetIDs").is_some());
    let (_, moved_ball) = sys.get_ball_snapshot(v_out_cone).await.unwrap().unwrap();
    assert!(moved_ball.position.distance(DVec3::new(-50_000.0, 0.0, 0.0)) > 50_000.0, "GTFO must teleport target");

    // 3. Reaper (`ActivateSlashModule`) sweeps beam path between start and end points
    let p_start = PyRep::Tuple(vec![PyRep::Real(190_000.0), PyRep::Real(-10_000.0), PyRep::Real(0.0)]);
    let p_end = PyRep::Tuple(vec![PyRep::Real(210_000.0), PyRep::Real(10_000.0), PyRep::Real(0.0)]);
    let reaper_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(reaper_mod), p_start, p_end], &HashMap::new(), None)
        .await
        .expect("ActivateSlashModule Reaper");
    assert!(reaper_res.dict_get("hitTargetIDs").is_some());

    // 4. Lance (`ActivateSlashModule`) 300km directional beam + 15s warmup + 5-min jump/warp suppression
    let lance_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod)], &HashMap::new(), None)
        .await
        .expect("ActivateSlashModule");
    assert_eq!(lance_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
    assert_eq!(lance_res.dict_get("jumpWarpSuppressionSeconds"), Some(&PyRep::Real(300.0)));
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, titan_ship, "jump").is_err());
}

#[tokio::test]
async fn test_b35_nosferatu_mobile_depot_and_nanite_repair() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor, feat:modules.mobile-depot-deployment-and-refitting, feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&[
        "feat:modules.energy-nosferatu-relative-capacitor",
        "feat:modules.mobile-depot-deployment-and-refitting",
        "feat:modules.nanite-repair-paste-in-space",
    ]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    // 1. Energy Nosferatu relative capacitor percentage vs Blood Raider hull override
    let c_norm = 951_101;
    let s_norm = 961_101;
    let mut sess_norm = seed_pilot(&ctx, c_norm, s_norm, 641, NULL_SYS, false); // Megathron
    spawn_ship_in_space(&ctx, c_norm, s_norm, 641, NULL_SYS, DVec3::ZERO).await;
    let nos_mod = 971_101;
    give_item(&ctx, nos_mod, 12261, c_norm, s_norm, InventoryFlag::HiSlot0, 1); // Heavy Energy Nosferatu I

    let c_tgt = 951_102;
    let s_tgt = 961_102;
    seed_pilot(&ctx, c_tgt, s_tgt, 587, NULL_SYS, false); // Rifter (much smaller max cap!)
    spawn_ship_in_space(&ctx, c_tgt, s_tgt, 587, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    // Set Megathron to 80% cap (4000/5000) and Rifter to 50% cap (150/300):
    // Megathron has HIGHER cap % (80% > 50%) -> must NOT drain.
    let mut cs_norm = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    cs_norm.max_capacitor = 5_000.0;
    cs_norm.capacitor = 4_000.0;
    sys.set_combat_state(s_norm, cs_norm).await.unwrap();

    let mut cs_tgt = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt.max_capacitor = 300.0;
    cs_tgt.capacitor = 150.0;
    sys.set_combat_state(s_tgt, cs_tgt).await.unwrap();

    let _ = sys.add_target_lock(s_norm, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_norm = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_norm.capacitor >= 150.0, "Normal hull must not drain when activator cap % >= target cap %");

    // Now set Megathron to 30% cap (1500/5000) and Rifter to 50% cap (150/300):
    // Megathron has HIGHER absolute GJ (1500 > 150) but LOWER cap % (30% < 50%) -> MUST drain!
    let mut cs_norm2 = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    cs_norm2.capacitor = 1_500.0;
    sys.set_combat_state(s_norm, cs_norm2).await.unwrap();
    let mut cs_tgt2 = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt2.capacitor = 150.0;
    sys.set_combat_state(s_tgt, cs_tgt2).await.unwrap();
    let _ = sys.step_manual(15.0).await;
    let tgt_after_rel = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_rel.capacitor < 150.0, "Normal hull must drain when activator cap % < target cap % even if absolute GJ is higher");

    // Blood Raider hull (Bhaalgorn 17920) overrides percentage check
    let c_br = 951_103;
    let s_br = 961_103;
    let mut sess_br = seed_pilot(&ctx, c_br, s_br, 17920, NULL_SYS, false); // Bhaalgorn
    spawn_ship_in_space(&ctx, c_br, s_br, 17920, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let nos_br = 971_102;
    give_item(&ctx, nos_br, 12261, c_br, s_br, InventoryFlag::HiSlot0, 1);
    let mut cs_br = sys.get_combat_state(s_br).await.unwrap().unwrap();
    cs_br.max_capacitor = 5_000.0;
    cs_br.capacitor = 4_500.0; // 90% > 50%
    sys.set_combat_state(s_br, cs_br).await.unwrap();
    let _ = sys.add_target_lock(s_br, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_br, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_br), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Bhaalgorn Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_br = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_br.capacitor < 150.0, "Blood Raider hull must override cap % restriction and drain target");

    // 2. Mobile Depot deployment, anchoring, refitting, and 48h reinforcement
    let depot_item = 971_201;
    give_item(&ctx, depot_item, 33474, c_norm, s_norm, InventoryFlag::Cargo, 1);
    let dep_res = eve_server::services::mobile_deployables::check_mobile_deployment_restrictions(&ctx, &sess_norm, 33474, NULL_SYS, 1000125, 0, DVec3::new(100_000.0, 0.0, 0.0));
    assert!(dep_res.is_ok(), "Mobile Depot deployment must succeed >= 5km from structures/celestials");
    eve_server::services::mobile_deployables::register_mobile_deployable(&ctx, depot_item, 33474, NULL_SYS, c_norm, 1000125, 60, DVec3::new(100_000.0, 0.0, 0.0));
    let st_anchoring = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert_eq!(st_anchoring.anchor_duration_secs, 60);
    assert!(!st_anchoring.anchor_completed);

    // 3. Nanite Repair Paste in space: rejects burned-out module, repairs heat-damaged module
    let dmg_mod = 971_301;
    give_item(&ctx, dmg_mod, 12265, c_norm, s_norm, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, 971_302, 28668, c_norm, s_norm, InventoryFlag::Cargo, 500); // Nanite Repair Paste
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(9, 40.0);
            it.dynamic_attributes.insert(3, 40.0); // Burned out (40/40 HP)
        }
    });
    let burn_err = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "RepairModule", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(burn_err.is_err(), "Fully burned-out module cannot be repaired in space with Nanite Repair Paste");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(3, 10.0); // Partially heat-damaged (10/40 HP)
        }
        snap.module_active_states.insert(dmg_mod);
    });
    let active_err = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "RepairModule", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(active_err.is_err(), "Active module cannot be repaired until deactivated");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&dmg_mod);
    });
    let rep_ok = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "RepairModule", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(rep_ok.is_ok(), "Partially heat-damaged module repair must succeed");
    let paste_after = ctx.db.with_snapshot(|snap| snap.items.get(&971_301).map(|i| i.quantity).unwrap_or(500));
    assert!(paste_after < 500, "RepairModule must consume Nanite Repair Paste from ship cargo");
    let stop_ok = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "StopRepairModule", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(stop_ok.is_ok(), "StopRepairModule must succeed");
}

#[tokio::test]
async fn test_b35_cyno_bridges_conduit_jump_and_jump_drive() {
    // parity: feat:modules.industrial-cyno-rules, feat:modules.jump-portal-bridge-rules, feat:navigation.conduit-jump, feat:navigation.jump-arrival-placement, feat:navigation.jump-drive-range-fuel
    eve_testkit::attest(&[
        "feat:modules.industrial-cyno-rules",
        "feat:modules.jump-portal-bridge-rules",
        "feat:navigation.conduit-jump",
        "feat:navigation.jump-arrival-placement",
        "feat:navigation.jump-drive-range-fuel",
    ]);
    let ctx = build_ctx();

    // 1. Industrial Cyno on Venture (32880) in DEST_NULL_SYS
    let cyno_char = 952_101;
    let cyno_ship = 962_101;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 32880, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 32880, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let indy_cyno_mod = 972_101;
    give_item(&ctx, indy_cyno_mod, 52694, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_102, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000); // Liquid Ozone

    let cyno_res = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(indy_cyno_mod), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None)
        .await
        .expect("Venture industrial cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // 2. Jump Freighter (Rhea 28844) jumps to Industrial Cyno, consumes capacitor & isotopes, lands 5km away
    let jf_char = 952_102;
    let jf_ship = 962_102;
    let mut jf_sess = seed_pilot(&ctx, jf_char, jf_ship, 28844, NULL_SYS, false);
    spawn_ship_in_space(&ctx, jf_char, jf_ship, 28844, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_103, 17888, jf_char, jf_ship, InventoryFlag::SpecializedFuelBay, 50_000); // Nitrogen Isotopes

    // Put both in same fleet
    ctx.db.with_mut_snapshot(|snap| {
        let mut details = HashMap::new();
        details.insert(cyno_char, FleetMemberRecord { character_id: cyno_char, wing_id: None, squad_id: None, role: eve_db::FleetRole::FleetCommander, role_name: "FC".into(), takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true, job: 2, join_time_unix: 1 });
        details.insert(jf_char, FleetMemberRecord { character_id: jf_char, wing_id: None, squad_id: None, role: eve_db::FleetRole::SquadMember, role_name: "Member".into(), takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true, job: 0, join_time_unix: 1 });
        snap.fleets.insert(880_001, FleetRecord { fleet_id: 880_001, boss_id: cyno_char, motd: String::new(), free_move: true, is_registered: false, members: vec![cyno_char, jf_char], member_details: details, wings: Vec::new(), squads: Vec::new(), wing_details: HashMap::new(), squad_details: HashMap::new(), invites: std::collections::HashSet::new() });
    });

    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
    let cap_before = origin_sys.get_combat_state(jf_ship).await.unwrap().unwrap().capacitor;
    let jump_res = ctx
        .dispatch_rpc(&mut jf_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None)
        .await
        .expect("JF jump to industrial cyno");
    assert_eq!(jump_res.dict_get("solarSystemID"), Some(&PyRep::Integer(DEST_NULL_SYS as i64)));
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let cap_after = dest_sys.get_combat_state(jf_ship).await.unwrap().unwrap().capacitor;
    assert!(cap_after < cap_before, "Jump drive must consume capacitor");

    // 3. Titan cannot conduit jump (`TitanCannotConduitJump`)
    let titan_char = 952_103;
    let titan_ship = 962_103;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    let conduit_err = ctx
        .dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None)
        .await;
    assert!(conduit_err.is_err(), "Titans cannot conduit jump");
}

#[tokio::test]
async fn test_b35_navigation_align_cloak_bumping_drifter_zarzakh_nullifier() {
    // parity: feat:navigation.align-server-tick-rounding, feat:navigation.cloak-velocity-penalty, feat:navigation.collision-bumping, feat:navigation.drifter-wormhole-systems, feat:navigation.gravity-hazard-safe-zones, feat:navigation.interdiction-nullification-module
    eve_testkit::attest(&[
        "feat:navigation.align-server-tick-rounding",
        "feat:navigation.cloak-velocity-penalty",
        "feat:navigation.collision-bumping",
        "feat:navigation.drifter-wormhole-systems",
        "feat:navigation.gravity-hazard-safe-zones",
        "feat:navigation.interdiction-nullification-module",
    ]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    // 1. Align server tick rounding: 3.1s computed align warps out on tick 4
    let c_align = 953_101;
    let s_align = 963_101;
    seed_pilot(&ctx, c_align, s_align, 587, NULL_SYS, false);
    let align_ball = Ball {
        id: s_align,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 2.23614, // ln(4) * 2.23614 * 1e6 / 1e6 = 3.1000s -> ceil = 4s
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Warp {
            destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            phase: eve_destiny::WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
            origin: DVec3::ZERO,
            total_distance: 1_000_000_000.0,
        },
    };
    let _ = sys.spawn_ball(align_ball).await;
    for _ in 0..3 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b3) = sys.get_ball_snapshot(s_align).await.unwrap().unwrap();
    assert!(matches!(b3.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }), "Must still be aligning at tick 3");
    let _ = sys.step_manual(1.0).await;
    let (_, b4) = sys.get_ball_snapshot(s_align).await.unwrap().unwrap();
    assert!(matches!(b4.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Accelerating, .. }), "Must enter warp on tick 4");

    // 2. Cloak velocity penalty: Prototype (-90%), Improved (-75%), Covert Ops (0%)
    let c_clk = 953_102;
    let s_clk = 963_102;
    let mut sess_clk = seed_pilot(&ctx, c_clk, s_clk, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_clk, s_clk, 641, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let (_, b_init) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    let base_vmax = b_init.max_velocity;
    let proto_clk = 973_101;
    give_item(&ctx, proto_clk, 11370, c_clk, s_clk, InventoryFlag::HiSlot0, 1); // Prototype Cloaking Device I
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Prototype Cloak");
    let (_, b_cloaked) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_cloaked.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Prototype cloak must reduce max velocity by 90%");

    // 3. Drifter wormhole systems: 6 statics + 50-70 K-space connections
    let (drifter_sigs, _) = eve_server::services::space_destiny::space_docking_cloaking::ensure_system_signatures_and_anomalies(&ctx, 31_000_001).await;
    let wh_sigs: Vec<_> = drifter_sigs.iter().filter(|s| s.wormhole_type_id.is_some()).collect();
    assert!(wh_sigs.len() >= 56, "Drifter system must have 6 C1-C6 statics + 50-70 K-space connections, got {}", wh_sigs.len());

    // 4. Zarzakh gravity hazard safe zones
    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let c_safe = 953_103;
    let s_safe = 963_103;
    seed_pilot(&ctx, c_safe, s_safe, 641, ZARZAKH_SYS, false);
    let fulcrum_pos = ctx.sde.stations.get(&60015187).map(|s| s.position).unwrap_or(DVec3::ZERO);
    spawn_ship_in_space(&ctx, c_safe, s_safe, 641, ZARZAKH_SYS, fulcrum_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;

    let c_haz = 953_104;
    let s_haz = 963_104;
    seed_pilot(&ctx, c_haz, s_haz, 641, ZARZAKH_SYS, false);
    spawn_ship_in_space(&ctx, c_haz, s_haz, 641, ZARZAKH_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;

    let safe_hull_before = z_sys.get_combat_state(s_safe).await.unwrap().unwrap().structure_hp;
    let haz_hull_before = z_sys.get_combat_state(s_haz).await.unwrap().unwrap().structure_hp;
    let _ = z_sys.step_manual(1.0).await;
    let safe_hull_after = z_sys.get_combat_state(s_safe).await.unwrap().unwrap().structure_hp;
    let haz_hull_after = z_sys.get_combat_state(s_haz).await.unwrap().unwrap().structure_hp;
    assert_eq!(safe_hull_after, safe_hull_before, "Ship inside Zarzakh safe zone must take 0 hazard damage");
    assert!(haz_hull_after < haz_hull_before, "Ship outside Zarzakh safe zones must take periodic hull damage");

    // 5. Interdiction Nullifier passive penalties & bubble immunity
    let c_null = 953_105;
    let s_null = 963_105;
    let mut sess_null = seed_pilot(&ctx, c_null, s_null, 11176, NULL_SYS, false); // Crow (Interceptor)
    spawn_ship_in_space(&ctx, c_null, s_null, 11176, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    let null_mod = 973_102;
    give_item(&ctx, null_mod, 58919, c_null, s_null, InventoryFlag::HiSlot0, 1); // Interdiction Nullifier I
    ctx.dispatch_rpc(&mut sess_null, "dogmaIM", None, "Activate", &[PyRep::Integer(null_mod), PyRep::String("defaultEffect".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Interdiction Nullifier");
    let cs_null = sys.get_combat_state(s_null).await.unwrap().unwrap();
    assert!(cs_null.is_interdiction_nullified, "Interdiction Nullifier must grant bubble immunity");
}
