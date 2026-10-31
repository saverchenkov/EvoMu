//! Batch 39 open `feat:` parity integration tests (part 1: Mobile Cyno Beacon, Mobile Micro Jump Unit, Mobile Observatory).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{
    SKILL_ANCHORING, TYPE_COVERT_MOBILE_CYNO_BEACON, TYPE_MOBILE_CYNO_BEACON,
    TYPE_MOBILE_MJU, TYPE_MOBILE_OBSERVATORY,
};
use glam::DVec3;
use std::collections::HashMap;

fn dict_int_lookup(rep: &PyRep, key: i64) -> Option<i64> {
    match rep {
        PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(key)).and_then(|(_, v)| v.as_i64()),
        _ => None,
    }
}

fn kv_get(rep: &PyRep, key: &str) -> Option<PyRep> {
    match rep {
        PyRep::Dict(_) => rep.dict_get(key).cloned(),
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Object { args, .. } => args.dict_get(key).cloned(),
        _ => None,
    }
}

fn mk_item(id: i64, tid: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id: id, type_id: tid, owner_id: owner, location_id: loc, flag, quantity: qty, singleton, ..Default::default() }
}

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
    sess.user_id = Some(char_id as u64);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.security_status = 0.0;
        ch.balance = IskCents::from_isk(500_000_000);
        s.characters.insert(char_id, ch);
        s.items.insert(ship_id, mk_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
                max_velocity: 250.0, inertia: 0.5, mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0), speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_mobile_cyno_beacon_and_covert_variant() {
    // parity: feat:structures.mobile-cyno-beacon
    eve_testkit::attest(&["feat:structures.mobile-cyno-beacon"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let origin_sys = 30_000_014_u32; // Shamahi (2.10 LY from Asabona)
    let dest_sys = 30_000_012_u32;   // Asabona (low-sec)

    let (c1, s1) = (91_000_011_i64, 101_000_011_i64);
    let (c_carrier, s_carrier) = (91_000_012_i64, 101_000_012_i64);
    let (c_blops, s_blops) = (91_000_016_i64, 101_000_016_i64);
    let mut sess1 = setup_pilot(&ctx, c1, s1, 587, dest_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_carrier = setup_pilot(&ctx, c_carrier, s_carrier, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_blops = setup_pilot(&ctx, c_blops, s_blops, 22428, origin_sys, DVec3::new(505_000.0, 0.0, 0.0), 1_000_009).await;

    let (beacon_id, cov_beacon_id) = (101_000_099_i64, 101_000_100_i64);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(beacon_id, mk_item(beacon_id, TYPE_MOBILE_CYNO_BEACON, c1, s1, InventoryFlag::Cargo, 1, true));
        s.items.insert(cov_beacon_id, mk_item(cov_beacon_id, TYPE_COVERT_MOBILE_CYNO_BEACON, c1, s1, InventoryFlag::Cargo, 1, true));
        s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 2, skill_points: 10_000.0 });
        for (cid, sid, iso_tid, iid) in [(c_carrier, s_carrier, 17888_u32, 101_000_098_i64), (c_blops, s_blops, 16274_u32, 101_000_108_i64)] {
            s.skills.entry(cid).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
            s.skills.entry(cid).or_default().insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 90_510.0 });
            s.items.insert(iid, mk_item(iid, iso_tid, cid, sid, InventoryFlag::SpecializedFuelBay, 50_000, false));
        }
    });

    // 1. Negative cases: Anchoring II rejected & non-fleet pilot rejected
    let no_skill = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(no_skill.code, "SkillMissing");
    ctx.db.with_mut_snapshot(|s| { s.skills.get_mut(&c1).unwrap().get_mut(&SKILL_ANCHORING).unwrap().level = 3; });

    let no_fleet = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(no_fleet.code, "MustBeInFleet");

    for s in [&mut sess1, &mut sess_carrier, &mut sess_blops] {
        s.fleet_id = Some(777);
    }
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(777, eve_db::FleetRecord { fleet_id: 777, boss_id: c1, members: vec![c1, c_carrier, c_blops], ..Default::default() });
    });

    // 2. Deploy standard Mobile Cyno Beacon (57319): 1m spool + 60m active (despawnUnix - anchorUntilUnix == 3600), single-use
    let t0 = ctx.now_unix();
    let anchor_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.expect("Deploy Mobile Cyno Beacon");
    let despawn0 = kv_get(&anchor_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
    assert!(((t0 + 3_660) as i64..=(t0 + 3_662) as i64).contains(&despawn0));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

    // During 1m spool: not counted on map.GetBeaconCount and jump fails
    let map_spool = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    assert!(dict_int_lookup(&map_spool, dest_sys as i64).is_none());
    let j_args = [PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)];
    assert_eq!(ctx.dispatch_rpc(&mut sess_carrier, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");

    // After 61s spool: counted on map, fleet carrier jumps, non-fleet carrier rejected
    ctx.advance_secs(61);
    let map_active = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    assert_eq!(dict_int_lookup(&map_active, dest_sys as i64), Some(1));

    let mut sess_out = setup_pilot(&ctx, 91_000_013, 101_000_013, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_010).await;
    assert_eq!(ctx.dispatch_rpc(&mut sess_out, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");
    ctx.dispatch_rpc(&mut sess_carrier, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.expect("Fleet carrier jumps");

    // Early destruction via HP damage removes beacon and blocks subsequent jumps
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let norm_cs = dest_handle.get_combat_state(beacon_id).await.unwrap().unwrap();
    let _ = dest_handle.apply_damage(beacon_id, DamageVector { em: 200_000.0, thermal: 200_000.0, kinetic: 200_000.0, explosive: 200_000.0 }).await;
    assert!(dest_handle.get_ball_snapshot(beacon_id).await.unwrap().is_none());
    assert_eq!(ctx.dispatch_rpc(&mut sess_blops, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");

    // 3. Deploy Covert Mobile Cyno Beacon (59630, Tech II -> Anchoring V + Cynosural Field Theory V): 1m spool + 30m active, lower HP, hidden from map, Black Ops only
    let _ = dest_handle.set_ball_position(s1, DVec3::new(900_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(s1, [900_000.0, 0.0, 0.0]);
        let sk = s.skills.entry(c1).or_default();
        sk.insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 5, skill_points: 256_000.0 });
        sk.insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
    });
    let t_cov = ctx.now_unix();
    let cov_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(cov_beacon_id)], &kw, None).await.expect("Deploy Covert Mobile Cyno Beacon");
    let despawn_cov = kv_get(&cov_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
    assert!(((t_cov + 1_860) as i64..=(t_cov + 1_862) as i64).contains(&despawn_cov));

    let cov_cs = dest_handle.get_combat_state(cov_beacon_id).await.unwrap().unwrap();
    let norm_hp = norm_cs.max_shield_hp + norm_cs.max_armor_hp + norm_cs.max_structure_hp;
    let cov_hp = cov_cs.max_shield_hp + cov_cs.max_armor_hp + cov_cs.max_structure_hp;
    assert!(cov_hp < norm_hp, "Covert Mobile Cyno Beacon has lower total HP ({cov_hp} < {norm_hp})");

    ctx.advance_secs(61);
    let map_cov = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    assert!(dict_int_lookup(&map_cov, dest_sys as i64).is_none(), "Covert Mobile Cyno Beacon is hidden from map");

    let mut sess_dread = setup_pilot(&ctx, 91_000_017, 101_000_017, 19720, origin_sys, DVec3::new(510_000.0, 0.0, 0.0), 1_000_009).await;
    sess_dread.fleet_id = Some(777);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&777).unwrap().members.push(91_000_017);
        s.skills.entry(91_000_017).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
    });
    let cov_args = [PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_beacon_id)];
    assert_eq!(ctx.dispatch_rpc(&mut sess_dread, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &cov_args, &kw, None).await.unwrap_err().code, "ShipCannotJumpToBeacon");
    ctx.dispatch_rpc(&mut sess_blops, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &cov_args, &kw, None).await.expect("Black Ops jumps to Covert Mobile Cyno Beacon");
}

#[tokio::test]
async fn test_b39_mobile_micro_jump_unit_v24_component() {
    // parity: feat:structures.mobile-micro-jump-unit
    eve_testkit::attest(&["feat:structures.mobile-micro-jump-unit"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_id = 30_000_012_u32;
    let (c1, s1) = (91_000_021_i64, 101_000_021_i64);
    let mut sess1 = setup_pilot(&ctx, c1, s1, 587, sys_id, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let beyonce1 = ctx.bind_object(&mut sess1, "beyonce", sys_id as i64).unwrap();
    let handle = ctx.systems.get_or_spawn(sys_id);

    let mju_id = 101_000_097_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(mju_id, mk_item(mju_id, TYPE_MOBILE_MJU, c1, s1, InventoryFlag::Cargo, 1, true));
        s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 1, skill_points: 1_000.0 });
    });

    let t0 = ctx.now_unix();
    let anchor_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mju_id)], &kw, None).await.expect("Deploy MMJU");
    let despawn_mju = kv_get(&anchor_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
    assert!(((t0 + 60 + 2 * 86_400) as i64..=(t0 + 62 + 2 * 86_400) as i64).contains(&despawn_mju));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(mju_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(mju_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

    let mju_call = [PyRep::Integer(mju_id), PyRep::String("microJumpDriver".into()), PyRep::String("ActivateMicroJumpDrive".into())];
    ctx.advance_secs(30);
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.unwrap_err().code, "DeployableAnchoring");
    ctx.advance_secs(31);

    // Dynamic evaluated mass check: Charon (20185, base 960M kg < 1B) + online 25000mm Steel Plates I (40348, +80M kg -> 1.04B kg) rejected!
    let (c_charon, s_charon, plate_id) = (91_000_022_i64, 101_000_022_i64, 101_000_096_i64);
    let mut sess_charon = setup_pilot(&ctx, c_charon, s_charon, 20185, sys_id, DVec3::new(500_300.0, 0.0, 0.0), 1_000_009).await;
    let beyonce_charon = ctx.bind_object(&mut sess_charon, "beyonce", sys_id as i64).unwrap();
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(plate_id, mk_item(plate_id, 40348, c_charon, s_charon, InventoryFlag::LoSlot0, 1, true));
        s.module_online_states.insert(plate_id, true);
    });
    ctx.invalidate_ship_fitting(s_charon);
    assert_eq!(ctx.dispatch_rpc(&mut sess_charon, "beyonce", Some(beyonce_charon), "CallComponentFromClient", &mju_call, &kw, None).await.unwrap_err().code, "ShipTooHeavy");

    // Non-default facing directions (+Y on owner, -Z on hostile) + concurrent activation + 12s warm-up + zero use cooldown
    let (c_hostile, s_hostile) = (91_000_023_i64, 101_000_023_i64);
    let mut sess_hostile = setup_pilot(&ctx, c_hostile, s_hostile, 587, sys_id, DVec3::new(500_400.0, 0.0, 0.0), 1_000_011).await;
    let beyonce_hostile = ctx.bind_object(&mut sess_hostile, "beyonce", sys_id as i64).unwrap();

    ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess_hostile, "beyonce", Some(beyonce_hostile), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(-1.0)], &kw, None).await.unwrap();
    handle.step_manual(1.0).await.unwrap();

    let (_, s1_before) = handle.get_ball_snapshot(s1).await.unwrap().unwrap();
    let (_, sh_before) = handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Owner MMJU spool");
    ctx.dispatch_rpc(&mut sess_hostile, "beyonce", Some(beyonce_hostile), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Hostile MMJU spool");

    ctx.advance_secs(10);
    handle.step_manual(0.5).await.unwrap();
    let (_, s1_mid) = handle.get_ball_snapshot(s1).await.unwrap().unwrap();
    assert!((s1_mid.position - s1_before.position).length() < 1_000.0);

    ctx.advance_secs(3);
    handle.step_manual(0.5).await.unwrap();
    let d1 = handle.get_ball_snapshot(s1).await.unwrap().unwrap().1.position - s1_before.position;
    let dh = handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap().1.position - sh_before.position;
    assert!(d1.y > 99_000.0 && d1.x.abs() < 1_000.0, "Owner jumped 100 km along +Y: {d1:?}");
    assert!(dh.z < -99_000.0 && dh.x.abs() < 1_000.0, "Hostile jumped 100 km along -Z: {dh:?}");

    let mju_pos = eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&mju_id).unwrap().position;
    let _ = handle.set_ball_position(s1, DVec3::from_array(mju_pos)).await;
    ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Zero cooldown reuse");
}

#[tokio::test]
async fn test_b39_mobile_observatory_decloak_ping() {
    // parity: feat:structures.mobile-observatory-decloak-ping
    eve_testkit::attest(&["feat:structures.mobile-observatory-decloak-ping"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let (hisec_sys, wh_sys, abyss_sys, pochven_sys, lowsec_sys) = (30_000_142_u32, 31_000_007_u32, 32_000_001_u32, 30_000_021_u32, 30_000_012_u32);
    let (c1, s1, obs_id) = (91_000_031_i64, 101_000_031_i64, 101_000_090_i64);
    let mut sess = setup_pilot(&ctx, c1, s1, 587, hisec_sys, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;

    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 2, skill_points: 10_000.0 });
        s.items.insert(obs_id, mk_item(obs_id, TYPE_MOBILE_OBSERVATORY, c1, s1, InventoryFlag::Cargo, 1, true));
    });

    // 1. Anchoring II rejected + high-sec, wormhole, Abyssal, Pochven, and deadspace rejected
    let sk_err = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err();
    assert!(matches!(sk_err.code.as_str(), "SkillRequired" | "SkillMissing"));
    ctx.db.with_mut_snapshot(|s| { s.skills.get_mut(&c1).unwrap().get_mut(&SKILL_ANCHORING).unwrap().level = 3; });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotDeployInHighSec");

    for (rsys, code) in [(wh_sys, "CannotDeployInWormhole"), (abyss_sys, "CannotDeployInAbyssalSpace"), (pochven_sys, "CannotDeployInPochven")] {
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c1).unwrap().solar_system_id = rsys; s.items.get_mut(&s1).unwrap().location_id = rsys as i64; });
        let h = ctx.systems.get_or_spawn(rsys);
        let _ = h.spawn_ball(Ball { id: s1, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        sess.solar_system_id = Some(rsys);
        assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, code);
        let _ = h.remove_ball(s1).await;
    }

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c1).unwrap().solar_system_id = lowsec_sys; s.items.get_mut(&s1).unwrap().location_id = lowsec_sys as i64; });
    sess.solar_system_id = Some(lowsec_sys);
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = low_handle.spawn_ball_with_combat(
        Ball { id: s1, type_id: 587, position: DVec3::new(5_000_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(587),
    ).await;
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(lowsec_sys).or_default().push(eve_server::services::pve_exploration::PveSiteInstance { dungeon_id: 1234, position: [5_000_000_000.0, 0.0, 0.0], ..Default::default() });
    });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotDeployInDeadspace");
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| { reg.sites_by_system.remove(&lowsec_sys); });

    // 2. Activate real Prototype Cloaking Device I (11370) on 12 ships outside star radius and step 901s so their 15m cloak stabilization expires
    let mut cloaked_sessions = Vec::new();
    for i in 0..12_i64 {
        let (cid, sid, mid) = (91_001_100 + i, 101_001_100 + i, 101_002_100 + i);
        let mut csess = setup_pilot(&ctx, cid, sid, 11188, lowsec_sys, DVec3::new(5_100_000_000.0 + (i as f64) * 20_000.0, 0.0, 0.0), 1_000_010).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(mid, mk_item(mid, 11370, cid, sid, InventoryFlag::HiSlot0, 1, true));
            s.module_online_states.insert(mid, true);
        });
        ctx.invalidate_ship_fitting(sid);
        ctx.dispatch_rpc(&mut csess, "dogmaIM", None, "Activate", &[PyRep::Integer(mid)], &kw, None).await.expect("Cloak ship");
        cloaked_sessions.push((cid, sid, mid, csess));
    }
    for _ in 0..16 {
        low_handle.step_manual(60.0).await.unwrap();
    }

    // Deploy Observatory: 10m deploy + 2h active, cannot scoop or unanchor
    let t_dep = ctx.now_unix();
    let dep_rep = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.expect("Deploy Observatory");
    let despawn_obs = kv_get(&dep_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
    assert!(((t_dep + 7_800) as i64..=(t_dep + 7_802) as i64).contains(&despawn_obs));
    assert_eq!(ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "entity", None, "CmdUnanchor", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

    // 1 ship inside cloak stabilization timer + 1 gate-cloaked ship (both immune)
    ctx.advance_secs(540);
    let (c_stab, s_stab, m_stab) = (91_001_200_i64, 101_001_200_i64, 101_002_200_i64);
    let mut sess_stab = setup_pilot(&ctx, c_stab, s_stab, 11188, lowsec_sys, DVec3::new(5_200_000_000.0, 0.0, 0.0), 1_000_010).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(m_stab, mk_item(m_stab, 11370, c_stab, s_stab, InventoryFlag::HiSlot0, 1, true));
        s.module_online_states.insert(m_stab, true);
    });
    ctx.invalidate_ship_fitting(s_stab);
    ctx.dispatch_rpc(&mut sess_stab, "dogmaIM", None, "Activate", &[PyRep::Integer(m_stab)], &kw, None).await.unwrap();

    let s_gate = 101_001_201_i64;
    let mut cs_gate = ctx.build_type_combat_state(587);
    cs_gate.is_cloaked = true;
    cs_gate.gate_cloak_remaining_ticks = 50;
    let _ = low_handle.spawn_ball_with_combat(
        Ball { id: s_gate, type_id: 587, position: DVec3::new(5_220_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        cs_gate,
    ).await;

    low_handle.step_manual(1.0).await.unwrap();
    assert!(eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&obs_id).unwrap().last_ping_unix.is_none());

    ctx.mechanic_rng.set_seed(Some(42));
    for (cid, _, _, _) in &cloaked_sessions {
        let _ = ctx.pending_notifications.write().unwrap().remove(cid);
    }
    ctx.advance_secs(61);
    low_handle.step_manual(1.0).await.unwrap();

    assert!(low_handle.get_combat_state(s_stab).await.unwrap().unwrap().is_cloaked);
    assert!(low_handle.get_combat_state(s_gate).await.unwrap().unwrap().is_cloaked);

    let mut decloaked_idx = Vec::new();
    for (idx, (cid, sid, mid, _)) in cloaked_sessions.iter().enumerate() {
        let cs = low_handle.get_combat_state(*sid).await.unwrap().unwrap();
        if !cs.is_cloaked {
            decloaked_idx.push(idx);
            assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(mid)));
            assert!(cs.decloak_targeting_delay_remaining_s > 0.0);
            let notes = ctx.pending_notifications.write().unwrap().remove(cid).unwrap_or_default();
            assert!(notes.iter().any(|n| n.event == "OnGodmaShipEffect"));
            assert!(notes.iter().any(|n| n.event == "OnSpecialFX" && n.args.len() == 14));
        }
    }
    assert!(!decloaked_idx.is_empty() && decloaked_idx.len() < 12, "Decloaked {}/12 ships", decloaked_idx.len());

    let (_, _, d_mid, ref mut d_sess) = cloaked_sessions[decloaked_idx[0]];
    let recloak_err = ctx.dispatch_rpc(d_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(d_mid)], &kw, None).await.unwrap_err();
    assert!(matches!(recloak_err.code.as_str(), "ModuleReactivationDelay" | "ModuleInReactivationCooldown" | "CloakReactivationDelay"));

    ctx.advance_secs(600);
    low_handle.step_manual(1.0).await.unwrap();
    assert!(eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&obs_id).unwrap().last_ping_unix.unwrap() >= t_dep + 1_200);

    ctx.advance_secs(7_201);
    low_handle.step_manual(1.0).await.unwrap();
    assert!(low_handle.get_ball_snapshot(obs_id).await.unwrap().is_none());
}
