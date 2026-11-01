//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 4: ESS grid warp & module restrictions).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
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
        ..Default::default()
    }
}

async fn step_until_stopped(ctx: &ServerContext, sys_id: u32, ship_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_h.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) && b.velocity.length() < 0.5 {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_b36_ess_grid_warp_and_module_restrictions() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions
    eve_testkit::attest(&["feat:pve.ess-grid-warp-mwd-restrictions"]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status <= -0.1
                && s.solar_system_id < 31_000_000
                && ctx
                    .sde
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let stargate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .unwrap()
        .stargate_id as i64;

    let pilot_a = 100_001_i64;
    let ship_a = 960_001_i64;
    let frig_a = 960_003_i64;
    let mwd_id = 960_010_i64;
    let ab_id = 960_011_i64;
    let mjd_id = 960_012_i64;
    let cloak_id = 960_013_i64;
    let cyno_id = 960_014_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ca = snap.characters.values().next().cloned().expect("base char");
        ca.character_id = pilot_a;
        ca.user_id = 1;
        ca.solar_system_id = sys_id;
        ca.station_id = None;
        ca.active_ship_id = ship_a;
        ca.balance = IskCents(0);
        snap.characters.insert(pilot_a, ca);

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_001);
        st.dbs_multiplier = 1.35;

        snap.items.insert(ship_a, make_item(ship_a, 639, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(frig_a, make_item(frig_a, 587, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 12052, pilot_a, ship_a, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, 12056, pilot_a, ship_a, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(mjd_id, make_item(mjd_id, 33915, pilot_a, ship_a, InventoryFlag::MedSlot2, 1, true));
        snap.items.insert(cloak_id, make_item(cloak_id, 11370, pilot_a, ship_a, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(cyno_id, make_item(cyno_id, 21096, pilot_a, ship_a, InventoryFlag::HiSlot2, 1, true));
        snap.items.insert(960_019, make_item(960_019, 16273, pilot_a, ship_a, InventoryFlag::Cargo, 2_000, false));
        for mid in [mwd_id, mjd_id, cloak_id, cyno_id] {
            snap.module_online_states.insert(mid, true);
        }
        snap.module_online_states.insert(ab_id, false);
        let sk = snap.skills.entry(pilot_a).or_default();
        for sid in [3318_u32, 3413, 3418, 3426, 3432, 3449, 3450, 3454, 3455, 4385, 11579, 21603] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
        for tid in [639_u32, 12052, 12056, 33915, 11370, 21096] {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [182_u16, 183, 184] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let mut sess_a = SessionState::new(3612);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    // 1. ESS intrusion site & ballpark acceleration gate (V24 0-arg essMgr.GetDataForClientSolarSystem)
    let ess_info = kv_map(
        &ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert!(!ess_info.contains_key("essGateID"));
    let ess_id = ess_info["essID"].as_i64().unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ess_gate_ball = sys_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.type_id == 17_831)
        .expect("ESS acceleration gate ball in system ballpark");
    let ess_gate_id = ess_gate_ball.id;
    let gate_pos = ess_gate_ball.position;

    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 639,
                position: gate_pos + DVec3::new(300_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.12,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;

    // Outside 500 km no-cloak & 75 km warp bubble: MWD allowed; direct warp onto ESS grid blocked; warp to exterior ESS gate allowed!
    let ess_pos = sys_h.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(550_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("MWD allowed beyond 75 km");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None).await;

    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Warp to exterior ESS gate");
    step_until_stopped(&ctx, sys_id, ship_a).await;
    let gate_standoff = gate_pos + DVec3::new(500.0, 0.0, 0.0);
    let _ = sys_h.set_ball_position(ship_a, gate_standoff).await;
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(ship_a, gate_standoff.to_array());
    });

    // At ESS gate (200 km from ESS beacon, within 500 km no-cloak zone): Cloak blocked with EssIntrusionBubbleCloakBlocked!
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );

    // Hull class eligibility: Frigate (587), Marauder (28659), Mining Barge (17478), Industrial (648),
    // non-Tactical Destroyer (16242), Black Ops (22428), and Capital (23911) rejected; T3D (34317) admitted!
    for (bad_sid, bad_tid) in [(frig_a, 587_u32), (960_004, 28_659), (960_005, 17_478), (960_006, 648), (960_008, 16_242), (960_009, 22_428), (960_029, 23_911)] {
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(bad_sid, make_item(bad_sid, bad_tid, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
            s.characters.get_mut(&pilot_a).unwrap().active_ship_id = bad_sid;
        });
        let _ = sys_h.spawn_ball_with_combat(Ball { id: bad_sid, type_id: bad_tid, position: gate_standoff, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(bad_tid)).await;
        sess_a.ship_id = Some(bad_sid);
        assert_eq!(ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None).await.unwrap_err().code, "ShipRestrictedByEssGate");
    }
    let t3d_id = 960_007_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(t3d_id, make_item(t3d_id, 34_317, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        s.characters.get_mut(&pilot_a).unwrap().active_ship_id = t3d_id;
    });
    let _ = sys_h.spawn_ball_with_combat(Ball { id: t3d_id, type_id: 34_317, position: gate_standoff, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 2_000_000.0, radius: 65.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(34_317)).await;
    sess_a.ship_id = Some(t3d_id);
    ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None).await.expect("Tactical Destroyer (group 1305) admitted by ESS gate");

    // Warp-scrambled ship rejected; unscrambled ship initiates warp WITHOUT prematurely teleporting snap.item_positions!
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_a).unwrap().active_ship_id = ship_a);
    sess_a.ship_id = Some(ship_a);
    let _ = sys_h.set_ball_position(ship_a, gate_standoff).await;
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_a).await {
        cs.warp_scramble_strength = 2;
        let _ = sys_h.set_combat_state(ship_a, cs).await;
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_a).await {
        cs.warp_scramble_strength = 0;
        cs.is_cloaked = true;
        let _ = sys_h.set_combat_state(ship_a, cs).await;
    }
    ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Cruiser/Battleship enters ESS gate warp slide and decloaks");
    assert!(!sys_h.get_combat_state(ship_a).await.unwrap().unwrap().is_cloaked);

    // Pre-landing invariant (`B36-F06`): while warp slide is in flight, persisted DB position is still at exterior gate!
    let pre_land_db_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.item_positions[&ship_a]));
    assert!(
        pre_land_db_pos.distance(ess_pos) > 150_000.0,
        "DB item_positions must NOT prematurely teleport to ESS landing grid before warp completes: {pre_land_db_pos:?}"
    );
    assert!(matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // Post-landing invariant (`B36-F06`): once SystemActor finishes warp slide, DB item_positions updates to landing grid!
    step_until_stopped(&ctx, sys_id, ship_a).await;
    let post_land_db_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.item_positions[&ship_a]));
    assert!(
        post_land_db_pos.distance(ess_pos) < 5_000.0,
        "SystemActor must persist landed ship position inside ESS grid upon warp completion: {post_land_db_pos:?}"
    );

    // 2. Inside 75 km ESS bubble: Warp, MWD (T1 + meta/storyline), MJD, Cloak, Cyno, and Travel/Abyssal Filaments ALL refused; Afterburner allowed!
    let storyline_mwd_id = 960_020_i64;
    let needlejack_id = 960_021_i64;
    let abyssal_fil_id = 960_022_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(storyline_mwd_id, make_item(storyline_mwd_id, 5_975, pilot_a, ship_a, InventoryFlag::MedSlot3, 1, true));
        snap.items.insert(needlejack_id, make_item(needlejack_id, 50_008, pilot_a, ship_a, InventoryFlag::Cargo, 2, false));
        snap.items.insert(abyssal_fil_id, make_item(abyssal_fil_id, 47_765, pilot_a, ship_a, InventoryFlag::Cargo, 2, false));
        snap.module_online_states.insert(storyline_mwd_id, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(stargate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(storyline_mwd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(storyline_mwd_id), PyRep::String("online".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(storyline_mwd_id, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(needlejack_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    let cruiser_ess_id = 960_023_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cruiser_ess_id, make_item(cruiser_ess_id, 621, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.get_mut(&abyssal_fil_id).unwrap().location_id = cruiser_ess_id;
        snap.characters.get_mut(&pilot_a).unwrap().active_ship_id = cruiser_ess_id;
    });
    let _ = sys_h.spawn_ball_with_combat(Ball { id: cruiser_ess_id, type_id: 621, position: ess_pos + DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 10_000_000.0, radius: 120.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(621)).await;
    sess_a.ship_id = Some(cruiser_ess_id);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(abyssal_fil_id), PyRep::Integer(47_765)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&pilot_a).unwrap().active_ship_id = ship_a);
    sess_a.ship_id = Some(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMjdBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_id), PyRep::String("cynosuralGeneration".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCynoBlocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner allowed inside 75 km ESS bubble");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into())], &kw, None).await;

    // 3. `F-04` regression: FleetWarp / WarpFleet enforces the 75 km ESS bubble for destinations and fleet members!
    let pilot_b = 100_002_i64;
    let ship_b = 960_040_i64; // Rifter Frigate (587) outside ESS bubble at 400 km
    ctx.db.with_mut_snapshot(|snap| {
        let mut cb = snap.characters[&pilot_a].clone();
        cb.character_id = pilot_b;
        cb.user_id = 2;
        cb.active_ship_id = ship_b;
        snap.characters.insert(pilot_b, cb);
        snap.items.insert(ship_b, make_item(ship_b, 587, pilot_b, i64::from(sys_id), InventoryFlag::None, 1, true));
    });
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_b,
                type_id: 587,
                position: ess_pos + DVec3::new(400_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let mut sess_b = SessionState::new(3699);
    crate::common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess_b, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(pilot_b)], &kw, None)
        .await
        .expect("SelectCharacterID pilot_b");
    ctx.db.clock().advance_secs(15);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.station_id = None;
    sess_b.ship_id = Some(ship_b);

    let cf = ctx
        .dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    let fid = cf.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or_else(|| sess_a.fleet_id.unwrap());
    ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(pilot_b), PyRep::None, PyRep::None, PyRep::Integer(4)], &kw, None)
        .await
        .expect("Invite pilot_b");
    let fb_b = ctx.bind_object(&mut sess_b, "fleetObjectHandler", fid).unwrap();
    ctx.dispatch_rpc(&mut sess_b, "fleetObjectHandler", Some(fb_b), "AcceptInvite", &[PyRep::Integer(fid)], &kw, None)
        .await
        .expect("AcceptInvite pilot_b");

    // (a) FleetWarp onto the ESS beacon / ESS grid is rejected with EssIntrusionBubbleWarpBlocked (never warps Frigate ship_b onto ESS grid!)
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(ess_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_b).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // (b) When ALL fleet ships are inside the 75 km ESS bubble, FleetWarp out to stargate fails with EssIntrusionBubbleWarpBlocked!
    let _ = sys_h.set_ball_position(ship_b, ess_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(stargate_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // (c) Mixed fleet: FC ship_a inside 75 km ESS bubble + squadmate ship_b outside (400 km) ->
    // FleetWarp to exterior stargate skips ESS-bubbled ship_a and warps ONLY unbubbled ship_b!
    let _ = sys_h.set_ball_position(ship_b, ess_pos + DVec3::new(400_000.0, 0.0, 0.0)).await;
    let fw_mixed = ctx
        .dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(stargate_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("Mixed FleetWarp skips ESS-bubbled ship_a and warps exterior ship_b");
    assert_eq!(fw_mixed.dict_get("warpCount"), Some(&PyRep::Integer(1)));
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));
    assert!(matches!(
        sys_h.get_ball_snapshot(ship_b).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));
}
