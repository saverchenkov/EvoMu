//! Open `feat:` parity integration tests for Mobile Cyno Beacon, Mobile Micro Jump Unit, and Mobile Observatory.

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

fn dict_beacon_tuple(rep: &PyRep, key: i64) -> Option<(i64, i64)> {
    match rep {
        PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(key)).and_then(|(_, v)| match v {
            PyRep::Tuple(t) if t.len() == 2 => Some((t[0].as_i64()?, t[1].as_i64()?)),
            _ => None,
        }),
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
    assert!(dict_beacon_tuple(&map_spool, dest_sys as i64).is_none());
    let j_args = [PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)];
    assert_eq!(ctx.dispatch_rpc(&mut sess_carrier, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");

    // After 61s spool: counted on map, fleet carrier jumps, non-fleet carrier rejected
    ctx.advance_secs(61);
    let map_active = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    assert_eq!(dict_beacon_tuple(&map_active, dest_sys as i64), Some((1, 0)));

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
    assert!(dict_beacon_tuple(&map_cov, dest_sys as i64).is_none(), "Covert Mobile Cyno Beacon is hidden from map");

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

#[allow(unused_imports, dead_code)]
mod standup_structure_weapons {
    use super::*;

// Standup Structure Weapons (`feat:structures.standup-point-defense-battery`,
// `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::SafetyLevel;
use eve_server::services::structure_standup_weapons::TYPE_KEEPSTAR;
use glam::DVec3;
use std::collections::HashMap;

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
    solar_system_id: u32,
    position: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        s.characters.insert(char_id, ch);
        s.items.insert(ship_id, mk_item(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::None, 1, true));
    });

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: ship_type_id, position, velocity: DVec3::ZERO,
                max_velocity: 250.0, inertia: 0.5, mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0), speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_standup_point_defense_battery() {
    // parity: feat:structures.standup-point-defense-battery
    eve_testkit::attest(&["feat:structures.standup-point-defense-battery"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let (hisec_sys, lowsec_sys) = (30_000_144_u32, 30_000_012_u32);
    let (corp_id, ally_corp_id, alliance_id, hostile_corp) = (98_000_555_i64, 98_000_556_i64, 99_000_555_i64, 98_000_599_i64);
    let (gunner_id, gunner_ship, keepstar_id, pdb_mod_id, flak_item_id) = (91_000_031_i64, 101_000_031_i64, 101_000_500_i64, 101_000_501_i64, 101_000_505_i64);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, hisec_sys, DVec3::ZERO, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&gunner_id).unwrap().alliance_id = Some(alliance_id);
        s.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.alliance_id = Some(alliance_id);
            __corp.name = "Keepstar Corp".into();
            __corp.ticker = "KEEP".into();
            __corp.ceo_id = gunner_id;
            __corp.members = [gunner_id].into_iter().collect();
            __corp });
        s.corporations.insert(ally_corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = ally_corp_id;
            __corp.alliance_id = Some(alliance_id);
            __corp.name = "Alliance Mate Corp".into();
            __corp.ticker = "ALLY".into();
            __corp.ceo_id = gunner_id;
            __corp });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: hisec_sys,
            name: "Test Keepstar".into(), position: [0.0; 3], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, hisec_sys as i64, InventoryFlag::None, 1, true));
        s.items.insert(pdb_mod_id, mk_item(pdb_mod_id, 35926, corp_id, keepstar_id, InventoryFlag::HiSlot0, 1, true));
        s.module_online_states.insert(pdb_mod_id, false);
    });

    let hi_handle = ctx.systems.get_or_spawn(hisec_sys);
    let _ = hi_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
    });
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    // 1. Negative cases: offline module, high-sec, non-XL structure, no ammo
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "ModuleOffline");
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(pdb_mod_id, true); });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "CannotActivateInHighSec");

    let st_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&keepstar_id).unwrap().solar_system_id = lowsec_sys;
        s.structures.get_mut(&keepstar_id).unwrap().position = [st_pos.x, st_pos.y, st_pos.z];
        s.items.get_mut(&keepstar_id).unwrap().location_id = lowsec_sys as i64;
        s.characters.get_mut(&gunner_id).unwrap().solar_system_id = lowsec_sys;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = low_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "RequiresXLStructure");
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = TYPE_KEEPSTAR; });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "NoAmmoLoaded");

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(flak_item_id, mk_item(flak_item_id, 63195, corp_id, keepstar_id, InventoryFlag::SpecializedAmmoHold, 500, false));
    });

    // Spawn hostile carrier (23915) at 1,500m and launch a real Light Fighter squadron (Templar I, 40562)
    let (c_hostile, s_hostile, fighter_item) = (91_000_032_i64, 101_000_032_i64, 101_000_033_i64);
    let mut sess_hostile = setup_pilot(&ctx, c_hostile, s_hostile, 23915, lowsec_sys, st_pos + DVec3::new(1_500.0, 0.0, 0.0), hostile_corp).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(fighter_item, mk_item(fighter_item, 40562, c_hostile, s_hostile, InventoryFlag::FighterBay, 9, false));
    });
    ctx.dispatch_rpc(&mut sess_hostile, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item)], &kw, None).await.expect("Load fighter");
    ctx.dispatch_rpc(&mut sess_hostile, "fighterMgr", None, "LaunchFightersFromTubes", &[], &kw, None).await.expect("Launch fighter");
    let fighter_ball_id = ctx.active_fighters.read().unwrap().keys().copied().next().expect("fighter ball");

    let spawn_drone = |did: i64, x_off: f64| {
        let h = low_handle.clone();
        let cs = ctx.build_type_combat_state(2454);
        async move {
            let _ = h.spawn_ball_with_meta(
                Ball { id: did, type_id: 2454, position: st_pos + DVec3::new(x_off, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.5, mass: 5_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop },
                cs,
                eve_destiny::SlimItemMeta { group_id: 100, category_id: 18, owner_id: c_hostile, char_id: Some(c_hostile), corp_id: hostile_corp, alliance_id: -1, ..Default::default() },
            ).await;
        }
    };
    spawn_drone(101_000_034, 1_800.0).await;

    let (c_ally, s_ally) = (91_000_035_i64, 101_000_035_i64);
    let _ = setup_pilot(&ctx, c_ally, s_ally, 587, lowsec_sys, st_pos + DVec3::new(1_200.0, 0.0, 0.0), ally_corp_id).await;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c_ally).unwrap().alliance_id = Some(alliance_id); });
    let s_far = 101_000_036_i64;
    let _ = setup_pilot(&ctx, 91_000_036, s_far, 587, lowsec_sys, st_pos + DVec3::new(5_000.0, 0.0, 0.0), hostile_corp).await;

    // 2. Low-sec Safety::Green spares neutral capsuleer ship (`s_hostile`); Safety::Red hits ship + drone + fighter!
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Green); });
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB Green safety");
    let carrier_cs_green = low_handle.get_combat_state(s_hostile).await.unwrap().unwrap();
    assert_eq!(carrier_cs_green.shield_hp, carrier_cs_green.max_shield_hp, "Safety Green spares neutral capsuleer in low-sec");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&flak_item_id).unwrap().quantity), 300, "Consumed 200 Flak Rounds");

    ctx.advance_secs(13);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Red); });
    spawn_drone(101_000_037, 1_700.0).await;

    let cap_before = low_handle.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor;
    let fighter_hp_before = low_handle.get_combat_state(fighter_ball_id).await.unwrap().map(|c| c.shield_hp + c.armor_hp + c.structure_hp).unwrap_or(0.0);
    let red_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB Red safety");
    assert!(kv_get(&red_rep, "targetsHit").and_then(|v| v.as_i64()).unwrap_or(0) >= 3);

    assert!(low_handle.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor < cap_before);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&flak_item_id).unwrap().quantity), 100);
    let carrier_cs_red = low_handle.get_combat_state(s_hostile).await.unwrap().unwrap();
    assert!(carrier_cs_red.shield_hp < carrier_cs_red.max_shield_hp);
    let fighter_hp_after = low_handle.get_combat_state(fighter_ball_id).await.unwrap().map(|c| c.shield_hp + c.armor_hp + c.structure_hp).unwrap_or(0.0);
    assert!(fighter_hp_after < fighter_hp_before, "Hostile fighter squadron took omni pulse damage");

    let ally_cs = low_handle.get_combat_state(s_ally).await.unwrap().unwrap();
    assert_eq!(ally_cs.shield_hp, ally_cs.max_shield_hp, "Alliance mate spared");
    let far_cs = low_handle.get_combat_state(s_far).await.unwrap().unwrap();
    assert_eq!(far_cs.shield_hp, far_cs.max_shield_hp, "Out-of-range ship spared");
    assert!(ctx.crimewatch.with_crimewatch(|cw| cw.get_aggression(gunner_id, c_hostile, ctx.now_unix()).is_some()));

    // 3. Fatal ship kill via Standup Point Defense Battery routes through `execute_death_pipeline`
    let (c_fatal, s_fatal, cargo_loot_id) = (91_000_038_i64, 101_000_038_i64, 101_000_039_i64);
    let _ = setup_pilot(&ctx, c_fatal, s_fatal, 587, lowsec_sys, st_pos + DVec3::new(1_100.0, 0.0, 0.0), hostile_corp).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&flak_item_id).unwrap().quantity = 400;
        s.items.insert(cargo_loot_id, mk_item(cargo_loot_id, 34, c_fatal, s_fatal, InventoryFlag::Cargo, 100, false));
    });
    let mut fatal_cs = low_handle.get_combat_state(s_fatal).await.unwrap().unwrap();
    fatal_cs.shield_hp = 0.0;
    fatal_cs.armor_hp = 0.0;
    fatal_cs.structure_hp = 50.0;
    low_handle.set_combat_state(s_fatal, fatal_cs).await.unwrap();

    ctx.advance_secs(13);
    let kill_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB fatal shot");
    assert!(kv_get(&kill_rep, "destroyedCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    let (ship_gone, new_pod_id, has_wreck, has_km) = ctx.db.with_snapshot(|s| {
        let gone = !s.items.contains_key(&s_fatal);
        let pod = s.characters.get(&c_fatal).map(|c| c.active_ship_id).unwrap_or(0);
        let wreck = s.items.values().any(|i| i.owner_id == c_fatal && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186));
        let km = s.killmails.values().any(|k| k.victim_character_id == c_fatal && k.ship_type_id == 587);
        (gone, pod, wreck, km)
    });
    assert!(ship_gone, "Destroyed Rifter removed from WorldStateSnapshot.items");
    assert_ne!(new_pod_id, s_fatal, "Victim active_ship_id switched to ejected capsule");
    assert!(low_handle.get_ball_snapshot(new_pod_id).await.unwrap().is_some(), "Ejected capsule spawned in Ballpark");
    assert!(has_wreck && has_km, "Wreck and KillmailRecord generated by execute_death_pipeline");
}

#[tokio::test]
async fn test_b39_standup_guided_bomb_and_arcing_vorton() {
    // parity: feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let null_sys = 30_000_199_u32;
    let (corp_id, hostile_corp) = (98_000_655_i64, 98_000_699_i64);
    let (gunner_id, gunner_ship, bystander_id) = (91_000_131_i64, 101_000_131_i64, 91_000_139_i64);
    let (keepstar_id, bomb_mod_id, vorton_mod_id) = (101_000_600_i64, 101_000_602_i64, 101_000_603_i64);
    let st_pos = DVec3::new(10_000_000.0, 0.0, 0.0);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, null_sys, st_pos, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Keepstar Corp".into();
            __corp.ticker = "KEEP".into();
            __corp.ceo_id = gunner_id;
            __corp.members = [gunner_id].into_iter().collect();
            __corp });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: TYPE_KEEPSTAR, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Test Keepstar".into(), position: [st_pos.x, st_pos.y, st_pos.z], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, null_sys as i64, InventoryFlag::None, 1, true));
        for (mid, tid, flag) in [(bomb_mod_id, 35923_u32, InventoryFlag::HiSlot1), (vorton_mod_id, 35928_u32, InventoryFlag::HiSlot2)] {
            s.items.insert(mid, mk_item(mid, tid, corp_id, keepstar_id, flag, 1, true));
            s.module_online_states.insert(mid, true);
        }
        // Bystander personal hangar guided bombs MUST NOT be usable by the structure weapon!
        s.items.insert(101_000_699, mk_item(101_000_699, 37849, bystander_id, keepstar_id, InventoryFlag::Hangar, 50, false));
    });

    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = null_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
    });
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    let (s_bs, s_frig, s_oor) = (101_000_140_i64, 101_000_141_i64, 101_000_142_i64);
    let _ = setup_pilot(&ctx, 91_000_140, s_bs, 638, null_sys, st_pos + DVec3::new(30_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_141, s_frig, 587, null_sys, st_pos + DVec3::new(34_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_142, s_oor, 638, null_sys, st_pos + DVec3::new(1_500_000.0, 0.0, 0.0), hostile_corp).await;

    // 1. Guided Bomb Launcher negative checks: unlocked target, bystander hangar ammo ignored, out of range
    let b_bs_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_bs)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "TargetNotLocked");

    for tid in [s_bs, s_frig, s_oor] {
        let _ = null_handle.add_target_lock(keepstar_id, TargetLock { target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(2_000_000.0), locked: true }).await;
    }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "NoAmmoLoaded");

    let heavy_bomb_stack = 101_000_620_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(heavy_bomb_stack, mk_item(heavy_bomb_stack, 37849, corp_id, keepstar_id, InventoryFlag::HiSlot1, 5, false));
    });
    let b_oor_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_oor)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_oor_args, &kw, None).await.unwrap_err().code, "TargetOutOfRange");

    // 2. Launch Heavy Guided Bomb (37849) at Raven: spawns in-flight bomb Ball with 4M HP, detonates on impact with 20km AoE sig-scaled splash!
    let bs_shield_0 = null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp;
    let frig_shield_0 = null_handle.get_combat_state(s_frig).await.unwrap().unwrap().shield_hp;

    let launch_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.expect("Launch Heavy Guided Bomb");
    assert_eq!(kv_get(&launch_rep, "inFlight"), Some(PyRep::Bool(true)));
    let bomb_ball_id = kv_get(&launch_rep, "bombID").and_then(|v| v.as_i64()).expect("bombID");
    assert_eq!(null_handle.get_combat_state(bomb_ball_id).await.unwrap().unwrap().max_structure_hp, 4_000_000.0);
    assert_eq!(null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp, bs_shield_0);

    for _ in 0..15 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    assert!(null_handle.get_ball_snapshot(bomb_ball_id).await.unwrap().is_none());
    let bs_dmg = bs_shield_0 - null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp;
    let frig_dmg = frig_shield_0 - null_handle.get_combat_state(s_frig).await.unwrap().unwrap().shield_hp;
    assert!(bs_dmg > 0.0 && frig_dmg > 0.0, "20 km AoE splash hit both Raven and nearby Rifter");
    assert!(bs_dmg > frig_dmg * 2.0, "Signature scaling: Raven ({bs_dmg}) >> Rifter ({frig_dmg})");

    // Light Guided Bomb (37850) after 61s cycle
    ctx.advance_secs(61);
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&heavy_bomb_stack).unwrap().type_id = 37850; });
    let b_frig_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_frig)];
    let light_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_frig_args, &kw, None).await.expect("Launch Light Guided Bomb");
    let light_bomb_id = kv_get(&light_rep, "bombID").and_then(|v| v.as_i64()).unwrap();
    for _ in 0..15 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    assert!(null_handle.get_ball_snapshot(light_bomb_id).await.unwrap().is_none());

    // 3. Standup Arcing Vorton Projector: Keepstar-only, <=400km range, 20km jump radius, max 5 total targets, 30s immobilization, 600s cooldown
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = 35833; });
    let v_bs_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_bs)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_bs_args, &kw, None).await.unwrap_err().code, "CantFitToStructure");
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = TYPE_KEEPSTAR; });

    let v_oor_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_oor)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_oor_args, &kw, None).await.unwrap_err().code, "TargetOutOfRange");

    let v_primary = 101_000_710_i64;
    let _ = setup_pilot(&ctx, 91_000_710, v_primary, 24692, null_sys, st_pos + DVec3::new(100_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = null_handle.add_target_lock(keepstar_id, TargetLock { target_id: v_primary, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
    let mut cluster_ids = vec![v_primary];
    for idx in 0..6_i64 {
        let sid = 101_000_711 + idx;
        let _ = setup_pilot(&ctx, 91_000_711 + idx, sid, 24692, null_sys, st_pos + DVec3::new(100_000.0 + ((idx + 1) as f64) * 2_500.0, 0.0, 0.0), hostile_corp).await;
        cluster_ids.push(sid);
    }
    let outside_jump_id = 101_000_720_i64;
    let _ = setup_pilot(&ctx, 91_000_720, outside_jump_id, 24692, null_sys, st_pos + DVec3::new(145_000.0, 0.0, 0.0), hostile_corp).await;
    for &sid in cluster_ids.iter().chain(std::iter::once(&outside_jump_id)) {
        let mut cs = null_handle.get_combat_state(sid).await.unwrap().unwrap();
        cs.shield_hp = 10_000_000.0;
        cs.max_shield_hp = 10_000_000.0;
        null_handle.set_combat_state(sid, cs).await.unwrap();
    }

    ctx.advance_secs(61);
    null_handle.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let v_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(v_primary)];
    let vorton_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_args, &kw, None).await.expect("Fire Arcing Vorton");
    assert_eq!(kv_get(&vorton_rep, "targetsHit").and_then(|v| v.as_i64()), Some(5));

    let mut hop_losses = Vec::new();
    for &sid in &cluster_ids {
        let cs = null_handle.get_combat_state(sid).await.unwrap().unwrap();
        let lost = cs.max_shield_hp - cs.shield_hp;
        if lost > 0.0 {
            hop_losses.push(lost);
        }
    }
    assert_eq!(hop_losses.len(), 5, "Exactly 5 of 7 ships in cluster took chained beam damage");
    for w in hop_losses.windows(2) {
        assert!(w[1] < w[0], "Vorton arc damage attenuates per hop: {} -> {}", w[0], w[1]);
    }
    let out_cs = null_handle.get_combat_state(outside_jump_id).await.unwrap().unwrap();
    assert_eq!(out_cs.shield_hp, out_cs.max_shield_hp, "Target beyond 20 km jump radius spared");

    // During 30s immobilization (+10s): weapons, ReleaseControl, EjectFromStructure, and Undock all blocked
    ctx.advance_secs(10);
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "WeaponCycleImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureControl", None, "ReleaseControl", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureControl", None, "EjectFromStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");

    // After 30s immobilization expires (+25s -> +35s): Vorton still on 600s cooldown, ReleaseControl succeeds
    ctx.advance_secs(25);
    let cyc_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_args, &kw, None).await.unwrap_err();
    assert_eq!(cyc_err.code, "ModuleAlreadyCycling");
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "ReleaseControl", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("ReleaseControl after 30s");
}

#[tokio::test]
async fn test_b39_standup_falloff_bomb_safety_timers_and_kills() {
    // parity: feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let low_sys = 30_000_012_u32;
    let (corp_id, hostile_corp, neutral_corp) = (98_000_755_i64, 98_000_799_i64, 98_000_788_i64);
    let (gunner_id, gunner_ship, keepstar_id, bomb_mod_id, vorton_mod_id, bomb_stack) =
        (91_000_231_i64, 101_000_231_i64, 101_000_800_i64, 101_000_802_i64, 101_000_803_i64, 101_000_820_i64);
    let st_pos = DVec3::new(20_000_000.0, 0.0, 0.0);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, low_sys, st_pos, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Lowsec Keepstar".into();
            __corp.ticker = "LKS".into();
            __corp.ceo_id = gunner_id;
            __corp.members = [gunner_id].into_iter().collect();
            __corp });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: TYPE_KEEPSTAR, owner_corp_id: corp_id, solar_system_id: low_sys,
            name: "Lowsec Keepstar".into(), position: [st_pos.x, st_pos.y, st_pos.z], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, low_sys as i64, InventoryFlag::None, 1, true));
        for (mid, tid, flag) in [(bomb_mod_id, 35923_u32, InventoryFlag::HiSlot1), (vorton_mod_id, 35928_u32, InventoryFlag::HiSlot2)] {
            s.items.insert(mid, mk_item(mid, tid, corp_id, keepstar_id, flag, 1, true));
            s.module_online_states.insert(mid, true);
        }
        s.items.insert(bomb_stack, mk_item(bomb_stack, 37849, corp_id, keepstar_id, InventoryFlag::HiSlot1, 10, false));
    });
    let low_h = ctx.systems.get_or_spawn(low_sys);
    let _ = low_h.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
    });
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    // 1. Guided Bomb splash: SafetyLevel::Green spares neutral low-sec bystander & corp friendly, hits suspect + sets OF-51 PvP/logoff timers
    let (c_susp, s_susp, c_neut, s_neut, c_frnd, s_frnd) =
        (91_000_240_i64, 101_000_240_i64, 91_000_241_i64, 101_000_241_i64, 91_000_242_i64, 101_000_242_i64);
    let _ = setup_pilot(&ctx, c_susp, s_susp, 638, low_sys, st_pos + DVec3::new(25_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, c_neut, s_neut, 638, low_sys, st_pos + DVec3::new(28_000.0, 0.0, 0.0), neutral_corp).await;
    let _ = setup_pilot(&ctx, c_frnd, s_frnd, 638, low_sys, st_pos + DVec3::new(26_500.0, 0.0, 0.0), corp_id).await;
    let _ = low_h.add_target_lock(keepstar_id, TargetLock { target_id: s_susp, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(c_susp).suspect_flag_until_unix = ctx.now_unix() + 900;
    }
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Green); });
    let neut_sh0 = low_h.get_combat_state(s_neut).await.unwrap().unwrap().shield_hp;
    let frnd_sh0 = low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp;
    let susp_sh0 = low_h.get_combat_state(s_susp).await.unwrap().unwrap().shield_hp;

    let b_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_susp)];
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_args, &kw, None).await.expect("Launch Guided Bomb under Green safety at suspect");
    for _ in 0..15 {
        low_h.step_manual(1.0).await.unwrap();
    }
    assert!(low_h.get_combat_state(s_susp).await.unwrap().unwrap().shield_hp < susp_sh0, "Suspect took guided bomb impact damage");
    assert_eq!(low_h.get_combat_state(s_neut).await.unwrap().unwrap().shield_hp, neut_sh0, "SafetyLevel::Green spared neutral low-sec bystander from AoE splash");
    assert_eq!(low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp, frnd_sh0, "Friendly corp ship spared from AoE splash");
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_susp).copied().unwrap_or(0) > ctx.now_unix()), "Suspect victim received OF-51 PvP logoff timer");
    assert_eq!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0)), 0, "Spared neutral has no PvP timer");

    // Switch gunner to SafetyLevel::Red and make `s_neut` low-HP so Guided Bomb splash kills `s_neut` through `execute_death_pipeline`!
    ctx.advance_secs(61);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Red); });
    let mut neut_cs = low_h.get_combat_state(s_neut).await.unwrap().unwrap();
    neut_cs.shield_hp = 0.0;
    neut_cs.armor_hp = 0.0;
    neut_cs.structure_hp = 100.0;
    low_h.set_combat_state(s_neut, neut_cs).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_args, &kw, None).await.expect("Launch Guided Bomb under Red safety");
    for _ in 0..15 {
        low_h.step_manual(1.0).await.unwrap();
    }
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&s_neut)), "Guided Bomb splash fatal kill destroyed ship via execute_death_pipeline");
    assert!(ctx.db.with_snapshot(|s| s.killmails.values().any(|k| k.victim_character_id == c_neut && k.ship_type_id == 638)), "Killmail generated for Guided Bomb kill");
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0) > ctx.now_unix()), "Splashed neutral received OF-51 PvP logoff timer");
    assert_eq!(low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp, frnd_sh0, "Friendly corp ship still spared under Red safety");

    // 2. Arcing Vorton optimal (250km) + falloff (50km, max 350km) range check & 0.5x falloff attenuation
    let (s_375k, s_200k, s_300k) = (101_000_250_i64, 101_000_251_i64, 101_000_252_i64);
    let _ = setup_pilot(&ctx, 91_000_250, s_375k, 24692, low_sys, st_pos + DVec3::new(375_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_251, s_200k, 24692, low_sys, st_pos + DVec3::new(200_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_252, s_300k, 24692, low_sys, st_pos + DVec3::new(0.0, 300_000.0, 0.0), hostile_corp).await;
    for sid in [s_375k, s_200k, s_300k] {
        let _ = low_h.add_target_lock(keepstar_id, TargetLock { target_id: sid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
        let mut cs = low_h.get_combat_state(sid).await.unwrap().unwrap();
        cs.shield_hp = 10_000_000.0;
        cs.max_shield_hp = 10_000_000.0;
        low_h.set_combat_state(sid, cs).await.unwrap();
    }

    // Target locked at 375 km (inside Keepstar 400 km lock range, beyond Vorton 350 km optimal+2*falloff) -> TargetOutOfRange!
    low_h.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let v_375_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_375k)], &kw, None).await.unwrap_err();
    assert_eq!(v_375_err.code, "TargetOutOfRange");
    assert_eq!(low_h.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor, 600_000.0, "Out-of-range Vorton check consumes zero cap");

    let rep_opt = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_200k)], &kw, None).await.expect("Vorton at 200km optimal");
    let dmg_opt = kv_get(&rep_opt, "appliedDamage").and_then(|v| v.as_f64()).unwrap();

    ctx.advance_secs(605);
    low_h.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let rep_fall = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_300k)], &kw, None).await.expect("Vorton at 300km (optimal+falloff)");
    let dmg_fall = kv_get(&rep_fall, "appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(((dmg_fall / dmg_opt) - 0.5).abs() < 0.02, "At optimal + 1*falloff (300 km), Vorton damage attenuates by 0.5x: opt={dmg_opt}, fall={dmg_fall}");
}

}

#[allow(unused_imports, dead_code)]
mod clone_vat_bay_fuel_and_fleet_cloning {
    use super::*;

// Open `feat:` parity integration tests for Clone Vat Bay fuel, immobilization, fleet clone install, and CloneJump.

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    corp_id: i64,
    station_id: Option<i64>,
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
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.security_status = 0.0;
        ch.balance = IskCents::from_isk(500_000_000);
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: station_id.unwrap_or(sys_id as i64),
                flag: if station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None },
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if station_id.is_none() {
            s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        }
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = station_id;
    sess.structure_id = None;

    if station_id.is_none() {
        let ship_type = ctx.sde.types.get(&ship_type_id);
        let handle = ctx.systems.get_or_spawn(sys_id);
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 0.5,
                    mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                    radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
    }
    sess
}

#[tokio::test]
async fn test_b39_clone_vat_bay_titan_and_rorqual_install_and_jump() {
    // parity: feat:modules.clone-vat-bay-fuel
    eve_testkit::attest(&["feat:modules.clone-vat-bay-fuel"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_host = 30_000_012_u32; // Asabona (low-sec)
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;

    // 1. Negative cases: non-Titan/Rorqual hull rejected & insufficient Liquid Ozone rejected
    let c_subcap = 91_000_050_i64;
    let s_subcap = 101_000_050_i64;
    let mut sess_subcap = setup_pilot(&ctx, c_subcap, s_subcap, 24692, sys_host, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009, None).await;
    let bad_cvb = 101_000_700_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            bad_cvb,
            InventoryItemRecord {
                item_id: bad_cvb,
                type_id: 23735, // Clone Vat Bay I
                owner_id: c_subcap,
                location_id: s_subcap,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(bad_cvb, true);
    });
    let hull_err = ctx
        .dispatch_rpc(&mut sess_subcap, "dogmaIM", None, "Activate", &[PyRep::Integer(bad_cvb)], &kw, None)
        .await
        .expect_err("Non-Titan/Rorqual hull cannot activate Clone Vat Bay");
    assert_eq!(hull_err.code, "ShipCannotFitModule");

    // 2. Titan (Avatar 11567) AND Rorqual (28352): SpecializedFuelBay priority, single LO consumption, ship immobilization, fleet install + CloneJump!
    let c_titan = 91_000_051_i64;
    let s_titan = 101_000_051_i64;
    let c_rorq = 91_000_055_i64;
    let s_rorq = 101_000_055_i64;
    let c_mate = 91_000_052_i64;
    let s_mate = 101_000_052_i64;
    let c_outsider = 91_000_053_i64;
    let s_outsider = 101_000_053_i64;

    let titan_pos = DVec3::new(5_000_000_000.0, 0.0, 0.0);
    let rorq_pos = DVec3::new(5_000_500_000.0, 0.0, 0.0);
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_host, titan_pos, 1_000_009, None).await;
    let mut sess_rorq = setup_pilot(&ctx, c_rorq, s_rorq, 28352, sys_host, rorq_pos, 1_000_009, None).await;
    let mut sess_mate = setup_pilot(&ctx, c_mate, s_mate, 587, sys_host, titan_pos + DVec3::new(25_000.0, 0.0, 0.0), 1_000_009, None).await;
    let _sess_outsider = setup_pilot(&ctx, c_outsider, s_outsider, 587, sys_host, titan_pos + DVec3::new(40_000.0, 0.0, 0.0), 1_000_010, None).await;
    let beyonce_titan = ctx.bind_object(&mut sess_titan, "beyonce", sys_host as i64).unwrap();

    let cvb_titan = 101_000_701_i64;
    let cvb_rorq = 101_000_711_i64;
    let lo_fuel_bay = 101_000_702_i64;
    let lo_cargo = 101_000_703_i64;
    let lo_rorq = 101_000_712_i64;

    ctx.db.with_mut_snapshot(|s| {
        for cid in [c_mate, c_outsider] {
            s.skills.entry(cid).or_default().insert(
                24242, // Infomorph Psychology V
                TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
            );
        }
        s.fleets.insert(
            889,
            eve_db::FleetRecord {
                fleet_id: 889,
                boss_id: c_titan,
                members: vec![c_titan, c_rorq, c_mate],
                ..Default::default()
            },
        );
        for (mid, owner, ship) in [(cvb_titan, c_titan, s_titan), (cvb_rorq, c_rorq, s_rorq)] {
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 23735, // Clone Vat Bay I
                    owner_id: owner,
                    location_id: ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(mid, true);
        }
        // Start with 0 LO on Titan -> NotEnoughFuel
        s.items.insert(
            lo_fuel_bay,
            InventoryItemRecord {
                item_id: lo_fuel_bay,
                type_id: 16273,
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 0,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            lo_rorq,
            InventoryItemRecord {
                item_id: lo_rorq,
                type_id: 16273,
                owner_id: c_rorq,
                location_id: s_rorq,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    sess_titan.fleet_id = Some(889);
    sess_rorq.fleet_id = Some(889);
    sess_mate.fleet_id = Some(889);

    // Insufficient fuel check
    let no_fuel_err = ctx
        .dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
        .await
        .expect_err("Insufficient Liquid Ozone rejected");
    assert_eq!(no_fuel_err.code, "NotEnoughFuel");

    // Clone offer before activation is rejected
    let inactive_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(inactive_err.code, "NoActiveCloneVatBay");

    // Top up Titan SpecializedFuelBay (500 LO) + Cargo (500 LO) to verify SpecializedFuelBay priority & zero double-consumption
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&lo_fuel_bay).unwrap().quantity = 500;
        s.items.insert(
            lo_cargo,
            InventoryItemRecord {
                item_id: lo_cargo,
                type_id: 16273,
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
        .await
        .expect("Titan activates Clone Vat Bay I");
    ctx.dispatch_rpc(&mut sess_rorq, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_rorq)], &kw, None)
        .await
        .expect("Rorqual activates Clone Vat Bay I");

    let (fb_after_act, cargo_after_act, rorq_after_act) = ctx.db.with_snapshot(|s| {
        (
            s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0),
            s.items.get(&lo_cargo).map(|i| i.quantity).unwrap_or(0),
            s.items.get(&lo_rorq).map(|i| i.quantity).unwrap_or(0),
        )
    });
    assert!(fb_after_act < 500, "Titan consumed Liquid Ozone from SpecializedFuelBay ({fb_after_act} < 500)");
    assert_eq!(cargo_after_act, 500, "Cargo LO untouched when SpecializedFuelBay has enough fuel");
    assert!(rorq_after_act < 1_000, "Rorqual consumed Liquid Ozone ({rorq_after_act} < 1000)");

    // Ship immobilization enforced during active Clone Vat Bay cycle
    let sys_handle = ctx.systems.get_or_spawn(sys_host);
    let (_, titan_ball) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
    assert_eq!(titan_ball.max_velocity, 0.0, "Titan max_velocity set to 0 while Clone Vat Bay is active");
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            Some(beyonce_titan),
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await;
    let _ = warp_err;
    sys_handle.step_manual(2.0).await.unwrap();
    let (_, titan_ball_after_step) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
    assert!((titan_ball_after_step.position - titan_pos).length() < 1.0, "Immobilized Titan cannot move during cycle");

    // Non-fleet outsider cannot be offered a clone
    let non_fleet_offer_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_outsider), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(non_fleet_offer_err.code, "TargetNotInSameFleetOrSystem");

    // Fleetmate installs clone in Titan AND in Rorqual (verify ZERO extra LO deducted on AcceptShipCloneInstallation!)
    ctx.dispatch_rpc(
        &mut sess_titan,
        "jumpCloneSvc",
        None,
        "OfferShipCloneInstallation",
        &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
        &kw,
        None,
    )
    .await
    .expect("Offer clone on Titan");
    ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await
        .expect("Accept clone on Titan");

    let fb_after_install = ctx.db.with_snapshot(|s| s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(fb_after_install, fb_after_act, "AcceptShipCloneInstallation does NOT double-deduct Liquid Ozone");

    // Also install a second clone on the Rorqual
    let _ = sys_handle.set_ball_position(s_mate, rorq_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut sess_rorq,
        "jumpCloneSvc",
        None,
        "OfferShipCloneInstallation",
        &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
        &kw,
        None,
    )
    .await
    .expect("Offer clone on Rorqual");
    ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await
        .expect("Accept clone on Rorqual");

    let (titan_clone_id, rorq_clone_id) = ctx.db.with_snapshot(|s| {
        let clones = s.jump_clones.get(&c_mate).unwrap();
        let tc = clones.iter().find(|c| c.location_id == s_titan).unwrap().jump_clone_id;
        let rc = clones.iter().find(|c| c.location_id == s_rorq).unwrap().jump_clone_id;
        (tc, rc)
    });

    // 3. Dock fleetmate in Jita 4-4 NPC station (60_003_760) in a Capsule (670) and test `jumpCloneSvc.CloneJump` to the ship clone!
    let jita_pod_id = 101_000_999_i64;
    let _ = sys_handle.remove_ball(s_mate).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            jita_pod_id,
            InventoryItemRecord {
                item_id: jita_pod_id,
                type_id: 670,
                owner_id: c_mate,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = Some(jita_station);
        ch.structure_id = None;
        ch.active_ship_id = jita_pod_id;
        s.items.get_mut(&s_mate).unwrap().location_id = jita_station;
        s.items.get_mut(&s_mate).unwrap().flag = InventoryFlag::Hangar;
    });
    sess_mate.solar_system_id = Some(jita_sys);
    sess_mate.station_id = Some(jita_station);
    sess_mate.ship_id = Some(jita_pod_id);

    // Negative check: leaving fleet blocks `CloneJump` to ship clone!
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&889).unwrap().members.retain(|&m| m != c_mate);
    });
    sess_mate.fleet_id = None;
    let left_fleet_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(titan_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-fleet pilot cannot CloneJump into ship Clone Vat Bay");
    assert_eq!(left_fleet_err.code, "NotInHostShipFleet");

    // Rejoin fleet, set non-trivial corp/alliance/sec-status on jumper, and verify live Ballpark position + cloak checks + SlimItemMeta!
    let (mate_corp, mate_alliance) = (98_000_777_i64, 99_000_777_i64);
    let moved_titan_pos = DVec3::new(7_500_000_000.0, 250_000.0, -100_000.0);
    let _ = sys_handle.set_ball_position(s_titan, moved_titan_pos).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&889).unwrap().members.push(c_mate);
        // Deliberately leave stale `[0, 0, 0]` in `item_positions` to prove live Ballpark coordinates are used!
        s.item_positions.insert(s_titan, [0.0, 0.0, 0.0]);
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.corporation_id = mate_corp;
        ch.alliance_id = Some(mate_alliance);
        ch.security_status = 2.75;
    });
    sess_mate.fleet_id = Some(889);
    sess_mate.corporation_id = Some(mate_corp);

    // Negative check: Cloaked host Titan must block `CloneJump`!
    assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = true));
    let cloaked_jump_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(titan_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Cloaked host Titan must reject CloneJump");
    assert_eq!(cloaked_jump_err.code, "ShipCloaked");
    assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = false));

    ctx.dispatch_rpc(
        &mut sess_mate,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(titan_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("Fleet member CloneJumps to active uncloaked Titan Clone Vat Bay");

    assert_eq!(sess_mate.solar_system_id, Some(sys_host));
    assert_eq!(sess_mate.station_id, None, "Ship clone jump places pilot in space, not docked");
    assert_eq!(sess_mate.structure_id, None);
    let pod_id = sess_mate.ship_id.expect("spawned capsule ID");
    let pod_type = ctx.db.with_snapshot(|s| s.items.get(&pod_id).map(|i| i.type_id));
    assert_eq!(pod_type, Some(670), "Jumper spawned in Capsule (type 670)");
    let (_, pod_ball) = sys_handle.get_ball_snapshot(pod_id).await.unwrap().expect("Capsule ball in space");
    assert!((pod_ball.position - moved_titan_pos).length() < 10_000.0, "Capsule spawned adjacent to live Titan Ball coordinates, not stale [0,0,0]");
    let pod_meta = sys_handle.ballpark.read().unwrap().ball_slim_meta.get(&pod_id).cloned().expect("Capsule SlimItemMeta");
    assert_eq!(pod_meta.corp_id, mate_corp, "SlimItemMeta populated with jumper corporation_id");
    assert_eq!(pod_meta.alliance_id, mate_alliance, "SlimItemMeta populated with jumper alliance_id");
    assert!((pod_meta.security_status - 2.75).abs() < 1e-6, "SlimItemMeta populated with jumper security_status");

    // 4. Dock mate back in Jita, verify missing live Ballpark host ball rejects CloneJump even while DB CloneVatBayState is active, then verify cycle expiry!
    let _ = sys_handle.remove_ball(pod_id).await;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = Some(jita_station);
        ch.active_ship_id = jita_pod_id;
        ch.last_clone_jump_unix = 0;
    });
    sess_mate.solar_system_id = Some(jita_sys);
    sess_mate.station_id = Some(jita_station);
    sess_mate.ship_id = Some(jita_pod_id);
    ctx.advance_secs(15);

    let (_, saved_rorq_ball) = sys_handle.get_ball_snapshot(s_rorq).await.unwrap().unwrap();
    let saved_rorq_cs = sys_handle.get_combat_state(s_rorq).await.unwrap().unwrap();
    let _ = sys_handle.remove_ball(s_rorq).await;
    let missing_ball_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(rorq_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Missing live Ballpark host ship must block CloneJump");
    assert_eq!(missing_ball_err.code, "NoActiveCloneVatBay");
    let _ = sys_handle.spawn_ball_with_combat(saved_rorq_ball, saved_rorq_cs).await;

    ctx.advance_secs(305);
    sys_handle.step_manual(305.0).await.unwrap();

    let expired_offer_err = ctx
        .dispatch_rpc(
            &mut sess_rorq,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired Clone Vat Bay cycle blocks clone installation");
    assert_eq!(expired_offer_err.code, "NoActiveCloneVatBay");

    let expired_jump_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(rorq_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired Clone Vat Bay cycle blocks CloneJump");
    assert_eq!(expired_jump_err.code, "NoActiveCloneVatBay");
}

}

#[allow(unused_imports, dead_code)]
mod covert_cyno_and_jump_portal_generator {
    use super::*;

// Covert Cynosural Field & Covert Jump Portal Generator
// (`feat:modules.covert-cyno-rules`, `feat:modules.covert-jump-portal-generator`).

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn kv_get(rep: &PyRep, key: &str) -> Option<PyRep> {
    match rep {
        PyRep::Dict(_) => rep.dict_get(key).cloned(),
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Object { args, .. } => args.dict_get(key).cloned(),
        _ => None,
    }
}

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    position: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.item_positions.insert(ship_id, [position.x, position.y, position.z]);
    });

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 0.5,
                mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_covert_cyno_rules() {
    // parity: feat:modules.covert-cyno-rules
    eve_testkit::attest(&["feat:modules.covert-cyno-rules"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona (low-sec)
    let sys_b = 30_000_014_u32; // Shamahi (low-sec, 2.10 LY from Asabona)

    let c_cov = 91_000_041_i64;
    let s_cov = 101_000_041_i64; // Hound (12034)
    let c_prospect = 91_000_042_i64;
    let s_prospect = 101_000_042_i64; // Prospect (33697)
    let c_etana = 91_000_043_i64;
    let s_etana = 101_000_043_i64; // Etana (32790)
    let c_rabisu = 91_000_044_i64;
    let s_rabisu = 101_000_044_i64; // Rabisu (42245)
    let c_rifter = 91_000_045_i64;
    let s_rifter = 101_000_045_i64; // Rifter (587, non-covert)
    let c_offgrid = 91_000_046_i64;
    let s_offgrid = 101_000_046_i64; // 20,000 km away in sys_b (different grid bubble!)
    let c_ongrid = 91_000_047_i64;
    let s_ongrid = 101_000_047_i64; // 5 km away in sys_b (same grid bubble!)

    let mut sess_cov = setup_pilot(&ctx, c_cov, s_cov, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_prospect = setup_pilot(&ctx, c_prospect, s_prospect, 33697, sys_b, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_etana = setup_pilot(&ctx, c_etana, s_etana, 32790, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_rabisu = setup_pilot(&ctx, c_rabisu, s_rabisu, 42245, sys_b, DVec3::new(400_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_rifter = setup_pilot(&ctx, c_rifter, s_rifter, 587, sys_b, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_offgrid = setup_pilot(&ctx, c_offgrid, s_offgrid, 587, sys_b, DVec3::new(20_000_000.0, 0.0, 0.0), 1_000_010).await;
    let mut sess_ongrid = setup_pilot(&ctx, c_ongrid, s_ongrid, 587, sys_b, DVec3::new(105_000.0, 0.0, 0.0), 1_000_010).await;

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            888,
            eve_db::FleetRecord {
                fleet_id: 888,
                boss_id: c_cov,
                members: vec![c_cov, c_prospect, c_etana, c_rabisu, c_rifter],
                ..Default::default()
            },
        );
        for (idx, (cid, sid)) in [
            (c_cov, s_cov),
            (c_prospect, s_prospect),
            (c_etana, s_etana),
            (c_rabisu, s_rabisu),
            (c_rifter, s_rifter),
        ]
        .into_iter()
        .enumerate()
        {
            s.skills.entry(cid).or_default().insert(
                21603, // Cynosural Field Theory V
                TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 },
            );
            let mid = 101_000_800 + idx as i64;
            let loid = 101_000_810 + idx as i64;
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 28646, // Covert Cynosural Field Generator I
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(mid, true);
            s.items.insert(
                loid,
                InventoryItemRecord {
                    item_id: loid,
                    type_id: 16273, // Liquid Ozone
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 200,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });
    for sess in [&mut sess_cov, &mut sess_prospect, &mut sess_etana, &mut sess_rabisu, &mut sess_rifter] {
        sess.fleet_id = Some(888);
    }

    // 1. Requires Cynosural Field Theory V (CFT IV rejected) & fits only covert-cloak hulls + Etana/Prospect/Rabisu
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 4;
    });
    let sk_err = ctx
        .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(sk_err.code, "SkillMissing");
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 5;
    });

    let rifter_err = ctx
        .dispatch_rpc(&mut sess_rifter, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_804)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(rifter_err.code, "CannotLightCynoInSystem");

    // Strategic Cruiser without Covert Reconfiguration subsystem cannot light Covert Cyno, then WITH 45589 succeeds!
    let c_t3 = 91_000_048_i64;
    let s_t3 = 101_000_048_i64;
    let mut sess_t3 = setup_pilot(&ctx, c_t3, s_t3, 29984, sys_b, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;
    sess_t3.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_t3);
        s.skills.entry(c_t3).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_t3).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 });
        s.items.insert(101_000_809, InventoryItemRecord { item_id: 101_000_809, type_id: 28646, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        s.items.insert(101_000_819, InventoryItemRecord { item_id: 101_000_819, type_id: 16273, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default() });
        s.module_online_states.insert(101_000_809, true);
    });
    ctx.invalidate_ship_fitting(s_t3);
    assert_eq!(ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.unwrap_err().code, "CannotLightCynoInSystem");
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(101_000_829, InventoryItemRecord { item_id: 101_000_829, type_id: 45589, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::SubSystemSlot1, quantity: 1, singleton: true, ..Default::default() });
        s.module_online_states.insert(101_000_829, true);
    });
    ctx.invalidate_ship_fitting(s_t3);
    ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.expect("Tengu with Covert Reconfiguration lights Covert Cyno");

    // 2. Cyno-jammed system: normal cyno (21096) on Force Recon (Pilgrim 11965) fails with CynoJammed, while Covert Cyno succeeds!
    let c_pilgrim = 91_000_047_i64;
    let s_pilgrim = 101_000_047_i64;
    let mut sess_pilgrim = setup_pilot(&ctx, c_pilgrim, s_pilgrim, 11965, sys_b, DVec3::new(110_000.0, 0.0, 0.0), 1_000_009).await;
    sess_pilgrim.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_pilgrim);
        s.skills.entry(c_pilgrim).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(
            999_001,
            InventoryItemRecord {
                item_id: 999_001, type_id: 33700, owner_id: 1, location_id: sys_b as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_820,
            InventoryItemRecord {
                item_id: 101_000_820, type_id: 21096, owner_id: c_pilgrim, location_id: s_pilgrim,
                flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_821,
            InventoryItemRecord {
                item_id: 101_000_821, type_id: 16273, owner_id: c_pilgrim, location_id: s_pilgrim,
                flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default()
            },
        );
    });
    let norm_jam_err = ctx
        .dispatch_rpc(&mut sess_pilgrim, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_820)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(norm_jam_err.code, "CynoJammed");

    let cov_rep = ctx
        .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
        .await
        .expect("Hound lights Covert Cyno in cyno-jammed system");
    let cov_beacon_id = kv_get(&cov_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

    // Verify 1/10 cycle time (60s vs 600s) and 1/10 fuel (25 LO vs 250 LO at CFT V)
    assert_eq!(kv_get(&cov_rep, "durationSeconds").and_then(|v| v.as_f64()), Some(60.0));
    assert_eq!(kv_get(&cov_rep, "consumptionQuantity").and_then(|v| v.as_i64()), Some(25));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&101_000_810).unwrap().quantity), 175);

    // Prospect (33697), Etana (32790), and Rabisu (42245) all successfully light Covert Cyno!
    for (sess, mid) in [
        (&mut sess_prospect, 101_000_801_i64),
        (&mut sess_etana, 101_000_802_i64),
        (&mut sess_rabisu, 101_000_803_i64),
    ] {
        ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mid)], &kw, None)
            .await
            .expect("Special hull (Prospect/Etana/Rabisu) lights Covert Cyno");
    }

    // 3. On-grid-only visibility: hidden from `map.GetBeaconCount` and off-grid `scanMgr.ConeScan`, visible to on-grid `scanMgr.ConeScan`!
    let map_counts = ctx.dispatch_rpc(&mut sess_cov, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    let sys_b_entry = match &map_counts {
        PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(sys_b as i64)),
        _ => None,
    };
    assert!(sys_b_entry.is_none(), "Covert cyno hidden from system map beacon count");

    let au_m = 14.3 * 149_597_870_700.0;
    let offgrid_dscan = ctx
        .dispatch_rpc(&mut sess_offgrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
        .await
        .unwrap();
    let offgrid_list = match &offgrid_dscan {
        PyRep::List(l) => l,
        _ => panic!("ConeScan list"),
    };
    let offgrid_sees_beacon = offgrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
    assert!(!offgrid_sees_beacon, "Off-grid ship cannot see Covert Cyno on D-Scan");

    let ongrid_dscan = ctx
        .dispatch_rpc(&mut sess_ongrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
        .await
        .unwrap();
    let ongrid_list = match &ongrid_dscan {
        PyRep::List(l) => l,
        _ => panic!("ConeScan list"),
    };
    let ongrid_sees_beacon = ongrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
    assert!(ongrid_sees_beacon, "On-grid ship sees Covert Cyno on grid");

    // 4. Jump & bridge restrictions: ONLY Black Ops can jump or bridge to a Covert Cyno (Dreadnought & Titan rejected)
    let c_titan = 91_000_049_i64;
    let s_titan = 101_000_049_i64;
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let c_blops = 91_000_059_i64;
    let s_blops = 101_000_059_i64;
    let mut sess_blops = setup_pilot(&ctx, c_blops, s_blops, 22428, sys_a, DVec3::new(105_000.0, 0.0, 0.0), 1_000_009).await;
    sess_titan.fleet_id = Some(888);
    sess_blops.fleet_id = Some(888);

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.extend([c_titan, c_blops]);
        for cid in [c_titan, c_blops] {
            s.skills.entry(cid).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
            s.skills.entry(cid).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
        }
        // Standard Jump Portal Generator I (23953) on Titan
        s.items.insert(
            101_000_830,
            InventoryItemRecord {
                item_id: 101_000_830, type_id: 23953, owner_id: c_titan, location_id: s_titan,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        // Helium Isotopes on Black Ops
        s.items.insert(
            101_000_831,
            InventoryItemRecord {
                item_id: 101_000_831, type_id: 16274, owner_id: c_blops, location_id: s_blops,
                flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, singleton: false, ..Default::default()
            },
        );
    });

    let titan_jump_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

    let titan_bridge_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(titan_bridge_err.code, "ShipCannotBridgeToBeacon");

    ctx.dispatch_rpc(
        &mut sess_blops,
        "beyonce",
        None,
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Black Ops jumps to Covert Cynosural Field");
}

#[tokio::test]
async fn test_b39_covert_jump_portal_generator_all_hulls_and_fuel() {
    // parity: feat:modules.covert-jump-portal-generator
    eve_testkit::attest(&["feat:modules.covert-jump-portal-generator"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona
    let sys_b = 30_000_014_u32; // Shamahi (2.10 LY)

    let c_cyno = 91_000_101_i64;
    let s_cyno = 101_000_101_i64;
    let c_bo = 91_000_102_i64;
    let s_bo = 101_000_102_i64;
    let bo_pos = DVec3::new(100_000.0, 0.0, 0.0);

    let mut sess_cyno = setup_pilot(&ctx, c_cyno, s_cyno, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bo = setup_pilot(&ctx, c_bo, s_bo, 22428, sys_a, bo_pos, 1_000_009).await;

    // 6 eligible covert passenger hulls + 2 ineligible hulls (Tengu WITHOUT covert subsystem & Rifter)
    let passengers = [
        (91_000_111_i64, 101_000_111_i64, 12034_u32, true),  // Stealth Bomber (Hound, 1.455M kg)
        (91_000_112_i64, 101_000_112_i64, 11188_u32, true),  // Covert Ops Frigate (Anathema)
        (91_000_113_i64, 101_000_113_i64, 11965_u32, true),  // Force Recon (Pilgrim)
        (91_000_114_i64, 101_000_114_i64, 12729_u32, true),  // Blockade Runner (Crane, 11M kg)
        (91_000_115_i64, 101_000_115_i64, 29984_u32, true),  // Strategic Cruiser (Tengu WITH 45589 Covert Reconfiguration)
        (91_000_118_i64, 101_000_118_i64, 22428_u32, true),  // Black Ops (Redeemer)
        (91_000_116_i64, 101_000_116_i64, 29984_u32, false), // Strategic Cruiser WITHOUT covert subsystem -> rejected!
        (91_000_117_i64, 101_000_117_i64, 587_u32, false),   // Non-covert Rifter -> rejected!
    ];

    let mut p_sessions = Vec::new();
    for (idx, &(cid, sid, tid, _)) in passengers.iter().enumerate() {
        let mut ps = setup_pilot(
            &ctx,
            cid,
            sid,
            tid,
            sys_a,
            bo_pos + DVec3::new(300.0 + (idx as f64) * 100.0, 0.0, 0.0),
            1_000_009,
        )
        .await;
        ps.fleet_id = Some(999);
        p_sessions.push(ps);
    }
    sess_cyno.fleet_id = Some(999);
    sess_bo.fleet_id = Some(999);

    let cyno_mod = 101_000_901_i64;
    let portal_mod = 101_000_902_i64;
    let bo_fuel_bay_id = 101_000_903_i64;
    let bo_cargo_fuel_id = 101_000_904_i64;
    let tengu_cov_sub_id = 101_000_905_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut members = vec![c_cyno, c_bo];
        members.extend(passengers.iter().map(|p| p.0));
        s.fleets.insert(
            999,
            eve_db::FleetRecord { fleet_id: 999, boss_id: c_bo, members, ..Default::default() },
        );
        s.skills.entry(c_cyno).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_bo).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_bo).or_default().insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 90_510.0 }); // Jump Fuel Conservation IV
        for t3_cid in [91_000_115_i64, 91_000_116_i64] {
            s.skills.entry(t3_cid).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 }); // Caldari Defensive Systems V
        }

        s.items.insert(
            cyno_mod,
            InventoryItemRecord {
                item_id: cyno_mod, type_id: 28646, owner_id: c_cyno, location_id: s_cyno,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_910,
            InventoryItemRecord {
                item_id: 101_000_910, type_id: 16273, owner_id: c_cyno, location_id: s_cyno,
                flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default()
            },
        );
        s.items.insert(
            portal_mod,
            InventoryItemRecord {
                item_id: portal_mod, type_id: 28652, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        // Put Helium Isotopes (16274) in BOTH SpecializedFuelBay and Cargo to verify SpecializedFuelBay priority
        s.items.insert(
            bo_fuel_bay_id,
            InventoryItemRecord {
                item_id: bo_fuel_bay_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::SpecializedFuelBay, quantity: 100_000, singleton: false, ..Default::default()
            },
        );
        s.items.insert(
            bo_cargo_fuel_id,
            InventoryItemRecord {
                item_id: bo_cargo_fuel_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::Cargo, quantity: 20_000, singleton: false, ..Default::default()
            },
        );
        // Fit Tengu Defensive - Covert Reconfiguration (45589) ONLY on passenger #4 (101_000_115)
        s.items.insert(
            tengu_cov_sub_id,
            InventoryItemRecord {
                item_id: tengu_cov_sub_id,
                type_id: 45589,
                owner_id: 91_000_115,
                location_id: 101_000_115,
                flag: InventoryFlag::SubSystemSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(tengu_cov_sub_id, true);
    });
    ctx.invalidate_ship_fitting(101_000_115);
    ctx.invalidate_ship_fitting(101_000_116);

    let cyno_rep = ctx
        .dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
        .await
        .expect("Light Covert Cyno");
    let cov_beacon_id = kv_get(&cyno_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

    // Negative check: Covert Jump Portal Generator cannot bridge to a normal non-covert cyno!
    let c_norm = 91_000_199_i64;
    let s_norm = 101_000_199_i64;
    let mut sess_norm = setup_pilot(&ctx, c_norm, s_norm, 11965, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    sess_norm.fleet_id = Some(999);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&999).unwrap().members.push(c_norm);
        s.skills.entry(c_norm).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(101_000_998, InventoryItemRecord { item_id: 101_000_998, type_id: 21096, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        s.items.insert(101_000_999, InventoryItemRecord { item_id: 101_000_999, type_id: 16273, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default() });
    });
    let norm_rep = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_998)], &kw, None).await.unwrap();
    let norm_beacon_id = kv_get(&norm_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();
    let norm_bridge_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_norm), PyRep::Integer(sys_b as i64), PyRep::Integer(norm_beacon_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Covert Jump Portal Generator must NEVER bridge onto a normal cyno");
    assert_eq!(norm_bridge_err.code, "ShipCannotBridgeToBeacon");

    // Open Covert Jump Portal to Covert Cynosural Field
    ctx.dispatch_rpc(
        &mut sess_bo,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Black Ops opens Covert Jump Portal to Covert Cyno");

    // Non-covert Tengu (#6) and Rifter (#7) are rejected with JumpBridgeRestrictedShip
    for bad_idx in [6_usize, 7_usize] {
        let err = ctx
            .dispatch_rpc(
                &mut p_sessions[bad_idx],
                "beyonce",
                None,
                "CmdJumpThroughFleet",
                &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                &kw,
                None,
            )
            .await
            .expect_err("Non-covert hull must be rejected by Covert Jump Portal");
        assert_eq!(err.code, "JumpBridgeRestrictedShip");
    }

    // All 6 covert-capable hull classes (Stealth Bomber, Covert Ops, Force Recon, Blockade Runner, Covert Tengu, Black Ops) jump through!
    let mut fuels_by_hull = Vec::new();
    for ok_idx in 0..6_usize {
        let fb_before = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
        let jump_rep = ctx
            .dispatch_rpc(
                &mut p_sessions[ok_idx],
                "beyonce",
                None,
                "CmdJumpThroughFleet",
                &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("Eligible covert passenger #{ok_idx} failed: {e:?}"));
        let consumed = kv_get(&jump_rep, "fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        let fb_after = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
        assert_eq!((fb_before - fb_after) as i64, consumed, "Fuel deducted from Black Ops SpecializedFuelBay first");
        assert_eq!(p_sessions[ok_idx].solar_system_id, Some(sys_b));
        fuels_by_hull.push(consumed);
    }

    // Cargo isotopes untouched because SpecializedFuelBay had plenty, and heavier Crane (11M kg) used >7x more isotopes than Hound (1.455M kg)!
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bo_cargo_fuel_id).unwrap().quantity), 20_000);
    assert!(
        fuels_by_hull[3] > fuels_by_hull[0] * 6,
        "Isotope consumption scales with ship mass * LY: Crane={} vs Hound={}",
        fuels_by_hull[3],
        fuels_by_hull[0]
    );

    // Bubbled vs point-scrambled Black Ops host (`B39-REV-P0-01`):
    // Inside a warp disruption bubble (`warp_scramble_strength == 0`), jumping self is blocked (`WarpScrambled`),
    // whereas opening a Covert Jump Portal (`CmdBridgeToFleetModuleBeacon`) SUCCEEDS; targeted point-scramble blocks both!
    ctx.advance_secs(65);
    let cyno_rep2 = ctx.dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None).await.expect("Re-light Covert Cyno");
    let cov_beacon_id2 = kv_get(&cyno_rep2, "beaconID").and_then(|v| v.as_i64()).unwrap();
    let sys_a_h = ctx.systems.get_or_spawn(sys_a);
    assert!(sys_a_h.add_interdiction_sphere(101_000_977, bo_pos, 20_000.0).await.unwrap());
    assert!(sys_a_h.is_ball_scrambled(s_bo).await.unwrap(), "Black Ops is inside warp disruption bubble");
    let bubbled_jump_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
            &kw,
            None,
        )
        .await
        .expect_err("Self-jumping out of warp disruption bubble must fail");
    assert_eq!(bubbled_jump_err.code, "WarpScrambled");

    ctx.dispatch_rpc(
        &mut sess_bo,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
        &kw,
        None,
    )
    .await
    .expect("Non-point-scrambled Black Ops inside a warp disruption bubble opens Covert Jump Portal");

    sys_a_h.set_warp_scramble_strength(s_bo, 2).await.unwrap();
    let point_scram_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
            &kw,
            None,
        )
        .await
        .expect_err("Targeted warp scrambler must block Covert Jump Portal activation");
    assert_eq!(point_scram_err.code, "WarpScrambled");
}

}
