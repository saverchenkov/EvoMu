//! End-to-end V24 RPC integration tests for Step 4B Mobile Deployable Structures (Part 4):
//! - `feat:structures.mobile-cyno-beacon` (`57319`, `59630`)
//! - `feat:structures.mobile-micro-jump-unit` (`33591`)

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const LOWSEC_AMAMAKE: u32 = 30_002_537;
pub(crate) const LOWSEC_VARD: u32 = 30_002_538;
pub(crate) const HIGHSEC_JITA: u32 = 30_000_142;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

pub(crate) async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, bound_id, method, args, &kw, None)
        .await
}

pub(crate) fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

pub(crate) async fn spawn_pilot_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let uname = format!("pilot_{char_id}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: char_id as u64,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut tmpl = snap.characters[&90_000_002].clone();
            tmpl.user_id = char_id as u64;
            tmpl.character_id = char_id;
            tmpl.corporation_id = corp_id;
            tmpl.name = format!("Pilot {char_id}");
            snap.characters.insert(char_id, tmpl);
        }
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock_with_computer_hash(uid, sess_id, Some(char_id), None, true, ctx.now_unix());

    let (mass, radius) = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| (t.mass.max(1_000_000.0), t.radius.max(40.0)))
        .unwrap_or((10_000_000.0, 100.0));
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::new(100.0, 0.0, 0.0),
                max_velocity: 250.0,
                inertia: 3.0,
                mass,
                radius,
                speed_fraction: 0.4,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;

    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

pub(crate) fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let map = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in skills {
            map.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_mobile_cyno_beacon_and_covert_mobile_cyno_beacon_lifecycle_and_fleet_jump() {
    // parity: feat:structures.mobile-cyno-beacon
    eve_testkit::attest(&["feat:structures.mobile-cyno-beacon"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let deep_pos = DVec3::new(80_000_000.0, 80_000_000.0, 80_000_000.0);

    let owner_cid = 90_000_201;
    let fleet_cap_cid = 90_000_202;
    let fleet_blops_cid = 90_000_203;
    let nonfleet_corp_cid = 90_000_204;
    let corp_id = 98_002_200i64;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 701, owner_cid, corp_id, 810_001, 626, LOWSEC_VARD, deep_pos).await;
    let mut cap_sess =
        spawn_pilot_in_space(&ctx, 702, fleet_cap_cid, corp_id, 810_002, 23757, LOWSEC_AMAMAKE, deep_pos).await;
    let mut blops_sess =
        spawn_pilot_in_space(&ctx, 703, fleet_blops_cid, corp_id, 810_003, 22428, LOWSEC_AMAMAKE, deep_pos).await;
    let mut nonfleet_sess =
        spawn_pilot_in_space(&ctx, 704, nonfleet_corp_cid, corp_id, 810_004, 23757, LOWSEC_AMAMAKE, deep_pos).await;

    let std_beacon_id = 820_001i64;
    let cov_beacon_id = 820_002i64;
    let hs_beacon_id = 820_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc_ship) in [
            (std_beacon_id, 57319u32, 810_001i64),
            (cov_beacon_id, 59630u32, 810_001i64),
            (hs_beacon_id, 57319u32, 810_001i64),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner_cid,
                    location_id: loc_ship,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for (idx, ship_id) in [810_002i64, 810_003, 810_004].into_iter().enumerate() {
            snap.items.insert(
                825_000 + idx as i64,
                InventoryItemRecord {
                    item_id: 825_000 + idx as i64,
                    type_id: 16274,
                    owner_id: owner_cid + 1 + idx as i64,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 20_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Skill check: Anchoring III (11584 lvl 3) required for 57319
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );
    grant_skills(&ctx, owner_cid, &[(11584, 3)]);

    // 2. Fleet requirement: deploying without fleet fails with MustBeInFleet
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "MustBeInFleet"
    );

    let fleet_id = 77_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: owner_cid,
                members: vec![owner_cid, fleet_cap_cid, fleet_blops_cid],
                ..Default::default()
            },
        );
    });
    owner_sess.fleet_id = Some(fleet_id);
    cap_sess.fleet_id = Some(fleet_id);
    blops_sess.fleet_id = Some(fleet_id);

    // 3. High-sec check: move owner briefly to Jita -> CannotLightCynoInSystem
    owner_sess.solar_system_id = Some(HIGHSEC_JITA);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&owner_cid).unwrap().solar_system_id = HIGHSEC_JITA);
    let _ = ctx
        .systems
        .get_or_spawn(HIGHSEC_JITA)
        .spawn_ball(Ball {
            id: 810_001,
            type_id: 626,
            position: deep_pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(hs_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotLightCynoInSystem"
    );
    owner_sess.solar_system_id = Some(LOWSEC_VARD);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&owner_cid).unwrap().solar_system_id = LOWSEC_VARD);

    // 4. Deploy 57319 in low-sec Vard; verify single-use unanchor rejection
    let anchor_rep = rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
        .await
        .expect("Anchor Mobile Cynosural Beacon");
    assert_eq!(kv_get(&anchor_rep, "anchoringDelaySeconds"), Some(&PyRep::Real(60.0)));
    assert_eq!(
        kv_get(&anchor_rep, "despawnUnix"),
        Some(&PyRep::Integer((ctx.now_unix() + 3_660) as i64))
    );
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "entity", "CmdUnanchor", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnanchor"
    );

    // 5. During 60s spool: map.GetBeaconCount excludes it AND fleet jump returns NoActiveBeacon
    ctx.advance_secs(30);
    let PyRep::Dict(pre_cnt) = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap() else {
        panic!("expected dict");
    };
    assert!(!pre_cnt.iter().any(|(k, _)| k.as_i64() == Some(LOWSEC_VARD as i64)));
    assert_eq!(
        rpc(
            &ctx,
            &mut cap_sess,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveBeacon"
    );

    // 6. After 60s spool: map.GetBeaconCount includes Vard (+1), non-fleet corp mate is rejected, fleet Archon jumps!
    ctx.advance_secs(31);
    let post_counts = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap();
    let PyRep::Dict(cnt_entries) = post_counts else { panic!("expected dict") };
    assert!(cnt_entries.iter().any(|(k, v)| {
        k.as_i64() == Some(LOWSEC_VARD as i64)
            && (v.as_i64() == Some(1)
                || matches!(v, PyRep::Tuple(t) if t.first().and_then(|x| x.as_i64()) == Some(1)))
    }));

    assert_eq!(
        rpc(
            &ctx,
            &mut nonfleet_sess,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveBeacon",
        "Non-fleet corp mate must NOT see or jump to fleet-only Mobile Cynosural Beacon"
    );

    let jump_res = rpc(
        &ctx,
        &mut cap_sess,
        "beyonce",
        "CmdJumpToFleetDeployableBeacon",
        &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
    )
    .await
    .expect("Fleet Archon jumps to active Mobile Cynosural Beacon");
    assert_eq!(kv_get(&jump_res, "solarSystemID"), Some(&PyRep::Integer(LOWSEC_VARD as i64)));

    // 7. Covert Mobile Cynosural Beacon (59630): requires Anchoring V + Cynosural Field Theory V (21603 lvl 5),
    //    bypasses Mobile Cynosural Inhibitor (33476), stays hidden from map.GetBeaconCount, rejects Archon, allows Redeemer!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            820_099,
            InventoryItemRecord {
                item_id: 820_099,
                type_id: 33476,
                owner_id: nonfleet_corp_cid,
                location_id: LOWSEC_VARD as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(820_099, (deep_pos + DVec3::new(10_000.0, 0.0, 0.0)).to_array());
    });
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cov_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );
    grant_skills(&ctx, owner_cid, &[(11584, 5), (21603, 5)]);
    let cov_rep = rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cov_beacon_id)])
        .await
        .expect("Covert Mobile Cyno Beacon bypasses Mobile Cyno Inhibitor");
    assert_eq!(
        kv_get(&cov_rep, "despawnUnix"),
        Some(&PyRep::Integer((ctx.now_unix() + 1_860) as i64))
    );
    ctx.advance_secs(65);

    let cov_counts = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap();
    let PyRep::Dict(cov_cnt_entries) = cov_counts else { panic!("expected dict") };
    assert!(!cov_cnt_entries.iter().any(|(k, _)| k.as_i64() == Some(LOWSEC_VARD as i64)));

    let mut cap_sess2 =
        spawn_pilot_in_space(&ctx, 705, fleet_cap_cid, corp_id, 810_002, 23757, LOWSEC_AMAMAKE, deep_pos).await;
    cap_sess2.fleet_id = Some(fleet_id);
    assert_eq!(
        rpc(
            &ctx,
            &mut cap_sess2,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(cov_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipCannotJumpToBeacon"
    );

    let blops_jump = rpc(
        &ctx,
        &mut blops_sess,
        "beyonce",
        "CmdJumpToFleetDeployableBeacon",
        &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(cov_beacon_id)],
    )
    .await
    .expect("Black Ops Redeemer jumps to Covert Mobile Cynosural Beacon");
    assert_eq!(kv_get(&blops_jump, "solarSystemID"), Some(&PyRep::Integer(LOWSEC_VARD as i64)));

    ctx.advance_secs(1_810);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&cov_beacon_id)));
}

#[tokio::test]
async fn test_mobile_micro_jump_unit_component_spool_mass_scram_and_concurrent_use() {
    // parity: feat:structures.mobile-micro-jump-unit
    eve_testkit::attest(&["feat:structures.mobile-micro-jump-unit"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let base_pos = DVec3::new(90_000_000.0, 90_000_000.0, 90_000_000.0);

    let owner_cid = 90_000_211;
    let hostile_cid = 90_000_212;
    let scram_cid = 90_000_213;
    let heavy_cid = 90_000_214;
    let far_cid = 90_000_215;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 711, owner_cid, 98_002_211, 830_001, 626, LOWSEC_AMAMAKE, base_pos).await;
    let mut hostile_sess = spawn_pilot_in_space(
        &ctx,
        712,
        hostile_cid,
        98_002_212,
        830_002,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(1_000.0, 500.0, 0.0),
    )
    .await;
    let mut scram_sess = spawn_pilot_in_space(
        &ctx,
        713,
        scram_cid,
        98_002_213,
        830_003,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(800.0, -500.0, 0.0),
    )
    .await;
    let mut heavy_sess = spawn_pilot_in_space(
        &ctx,
        714,
        heavy_cid,
        98_002_214,
        830_004,
        23757,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(1_500.0, 0.0, 0.0),
    )
    .await;
    let mut far_sess = spawn_pilot_in_space(
        &ctx,
        715,
        far_cid,
        98_002_215,
        830_005,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(12_000.0, 0.0, 0.0),
    )
    .await;

    grant_skills(&ctx, owner_cid, &[(11584, 1)]);
    let mmju_id = 835_001i64;
    let mmju_too_close_id = 835_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [mmju_id, mmju_too_close_id] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 33591,
                    owner_id: owner_cid,
                    location_id: 830_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mmju_id)])
        .await
        .expect("Anchor MMJU");

    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mmju_too_close_id)])
            .await
            .unwrap_err()
            .code,
        "DeploymentTooCloseToSameType"
    );
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "entity", "CmdUnanchor", &[PyRep::Integer(mmju_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnanchor"
    );

    let comp_args = [
        PyRep::Integer(mmju_id),
        PyRep::String("microJumpDriver".into()),
        PyRep::String("StartMicroJumpDriveForShip".into()),
    ];
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "DeployableAnchoring"
    );

    ctx.advance_secs(61);

    assert_eq!(
        rpc(&ctx, &mut far_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    assert_eq!(
        rpc(&ctx, &mut heavy_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "ShipTooHeavy"
    );

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    let owner_pre = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_pre = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    let scram_pre = sys_h.get_ball_snapshot(830_003).await.unwrap().unwrap().1.position;

    for s in [&mut owner_sess, &mut hostile_sess, &mut scram_sess] {
        let rep = rpc(&ctx, s, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .expect("Concurrent MMJU StartMicroJumpDriveForShip");
        assert_eq!(kv_get(&rep, "spoolSeconds"), Some(&PyRep::Real(12.0)));
    }

    ctx.advance_secs(5);
    assert!((sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position - owner_pre).length() < 1_000.0);
    sys_h
        .add_interdiction_sphere(999_830, scram_pre, 500.0)
        .await
        .unwrap();
    assert_eq!(
        rpc(&ctx, &mut scram_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    ctx.advance_secs(8);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;

    let owner_post = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_post = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    let scram_post = sys_h.get_ball_snapshot(830_003).await.unwrap().unwrap().1.position;
    assert!(
        ((owner_post - owner_pre).length() - 100_000.0).abs() < 500.0,
        "Owner cruiser must translate 100 km (got {:.1} m)",
        (owner_post - owner_pre).length()
    );
    assert!(
        ((hostile_post - hostile_pre).length() - 100_000.0).abs() < 500.0,
        "Hostile cruiser using public MMJU must also translate 100 km (got {:.1} m)",
        (hostile_post - hostile_pre).length()
    );
    assert!(
        (scram_post - scram_pre).length() < 500.0,
        "Mid-spool scrambled cruiser must NOT jump (moved {:.1} m)",
        (scram_post - scram_pre).length()
    );

    // Mid-spool cloak and mid-spool unit expiry must both prevent jump on completion
    let _ = sys_h.set_ball_position(830_001, owner_pre).await;
    let _ = sys_h.set_ball_position(830_002, hostile_pre).await;
    for s in [&mut owner_sess, &mut hostile_sess] {
        rpc(&ctx, s, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .expect("Start second MMJU spool");
    }
    if let Ok(mut bp) = sys_h.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&830_001) {
            cs.is_cloaked = true;
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
            if let Some(rec) = st.deployables.get_mut(&mmju_id) {
                rec.expires_at_unix = ctx.now_unix() + 5;
            }
        });
    });
    ctx.advance_secs(13);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;
    let owner_cloaked_pos = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_expired_pos = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    assert!((owner_cloaked_pos - owner_pre).length() < 500.0, "Mid-spool cloaked ship must NOT jump");
    assert!((hostile_expired_pos - hostile_pre).length() < 500.0, "Ship on expired MMJU must NOT jump");
}
