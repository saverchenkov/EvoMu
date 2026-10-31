//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 3: Abyssal activation & Deadspace pockets).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{
    BookmarkRecord, FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const FIL_T1_ELEC: u32 = 47_765;

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
            if matches!(b.mode, MovementMode::Stop) {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_b36_abyssal_activation_and_deadspace_pocket_rules() {
    // parity: feat:pve.abyssal-activation-restrictions
    eve_testkit::attest(&["feat:pve.abyssal-activation-restrictions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 950_001_i64;
    let filament_id = 950_002_i64;

    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
                && !s.stations.is_empty()
                && ctx
                    .sde
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == s.solar_system_id)
        })
        .expect("low-sec system with station and stargate")
        .solar_system_id;
    let sta_pos = ctx.sde.solar_systems[&low_sys].stations[0].position;
    let gate_pos = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == low_sys)
        .unwrap()
        .position;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = low_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 621, char_id, i64::from(low_sys), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            filament_id,
            make_item(filament_id, FIL_T1_ELEC, char_id, ship_id, InventoryFlag::Cargo, 10, false),
        );
    });

    let mut sess = SessionState::new(3611);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(low_sys);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: sta_pos + DVec3::new(400_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(621),
        )
        .await;

    // 1. Within 1,000 km of NPC station and stargate -> ProximityRestriction
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    let _ = sys_h.set_ball_position(ship_id, gate_pos + DVec3::new(400_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    // 2. Warp via live CmdWarpToStuff to open space -> post-warp invulnerability blocks filament (ShipInvulnerable)
    let open_pos = DVec3::new(8.0e10, 8.0e10, 8.0e10);
    let bm_open = 950_090_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_open,
            BookmarkRecord {
                bookmark_id: bm_open,
                owner_id: char_id,
                label: "Deep Safe".into(),
                solar_system_id: low_sys,
                x: open_pos.x,
                y: open_pos.y,
                z: open_pos.z,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(bm_open)],
        &kw,
        None,
    )
    .await
    .expect("Warp to deep safe");
    step_until_stopped(&ctx, low_sys, ship_id).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInvulnerable"
    );
    for _ in 0..35 {
        let _ = sys_h.step_manual(1.0).await;
    }

    // 3. Within 1,000 km of a Mobile Depot -> ProximityRestriction
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: 950_010,
                type_id: 33_474,
                position: open_pos + DVec3::new(500_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(33_474),
        )
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    sys_h.try_remove_ball(950_010);

    // 3b. Within 1,000 km of an Upwell structure in snap.structures -> ProximityRestriction
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            950_020,
            UpwellStructureRecord {
                structure_id: 950_020,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: low_sys,
                name: "Test Astrahus".into(),
                position: (open_pos + DVec3::new(500_000.0, 0.0, 0.0)).to_array(),
                state: "ShieldVulnerable".into(),
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                fuel_blocks: 5_000,
                quantum_core_installed: true,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&950_020);
    });

    // 4. Safe-logoff timer -> LogoffTimerActive; 15m capsuleer PvP log-off timer -> PvpTimerActive
    ctx.advance_secs(301);
    ctx.dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None)
        .await
        .expect("SafeLogoff");
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "LogoffTimerActive"
    );
    ctx.dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &kw, None)
        .await
        .expect("AbortSafeLogoff");

    let gun_caracal = 950_003_i64;
    let enemy_ship = 950_090_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_caracal, make_item(gun_caracal, 2881, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(950_004, make_item(950_004, 21898, char_id, ship_id, InventoryFlag::HiSlot0, 500, false));
        snap.module_online_states.insert(gun_caracal, true);
        snap.items.insert(enemy_ship, make_item(enemy_ship, 587, 90_000_002, i64::from(low_sys), InventoryFlag::None, 1, true));
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3311, 3413, 3426] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: enemy_ship,
                type_id: 587,
                position: open_pos + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(90_000_002, enemy_ship, 587),
            ctx.build_ship_slim_meta(90_000_002, 587),
        )
        .await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(enemy_ship)], &kw, None).await.unwrap();
    for _ in 0..30 {
        if sys_h.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == enemy_ship && l.locked) {
            break;
        }
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_caracal), PyRep::Integer(enemy_ship)], &kw, None)
        .await
        .expect("Live PvP aggression shot");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gun_caracal)], &kw, None).await;
    sys_h.try_remove_ball(enemy_ship);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "PvpTimerActive"
    );
    ctx.advance_secs(901);

    // 5. Deploy 1st Abyssal Trace in open space (succeeds), then < 1,000 km from trace -> ProximityRestriction,
    // and activate trace gate via AbyssalEntranceGateActivation to enter Abyssal Deadspace (32_000_000..=32_999_999)
    let dep = kv_map(
        &ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .expect("Open space AbyssalEntranceDeployment"),
    );
    let trace_ball_id = dep["traceBallID"].as_i64().expect("traceBallID");
    let _ = sys_h.set_ball_position(ship_id, open_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    let _ = sys_h.set_ball_position(ship_id, open_pos).await;
    ctx.dispatch_rpc(
        &mut sess,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation enters Abyssal space");
    assert!(
        (32_000_000..=32_999_999).contains(&sess.solar_system_id.unwrap()),
        "Ship must transition into Abyssal solar system"
    );

    // 6. Real SDE-seeded Deadspace complex (DSG-30000142): acceleration gate entry, warp block, MWD SDE rule, and ShipInDungeon!
    let jita_sys = 30_000_142_u32;
    let rifter_id = 950_101_i64;
    let mwd_id = 950_102_i64;
    let ab_id = 950_103_i64;
    let fil_rifter = 950_104_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.active_ship_id = rifter_id;
        }
        snap.fleets.insert(
            701,
            FleetRecord {
                fleet_id: 701,
                boss_id: char_id,
                members: vec![char_id],
                motd: String::new(),
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        snap.items.insert(rifter_id, make_item(rifter_id, 587, char_id, i64::from(jita_sys), InventoryFlag::None, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 434, char_id, rifter_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, 439, char_id, rifter_id, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(950_105, make_item(950_105, 2881, char_id, rifter_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(950_106, make_item(950_106, 21898, char_id, rifter_id, InventoryFlag::HiSlot0, 2_000, false));
        snap.items.insert(fil_rifter, make_item(fil_rifter, FIL_T1_ELEC, char_id, rifter_id, InventoryFlag::Cargo, 6, false));
        snap.items.insert(950_107, make_item(950_107, 50_008, char_id, rifter_id, InventoryFlag::Cargo, 2, false));
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(ab_id, false);
        snap.module_online_states.insert(950_105, true);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3311, 3413, 3426, 3449, 3450, 3454, 3455] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(rifter_id);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(rifter_id);

    let jita_h = ctx.systems.get_or_spawn(jita_sys);
    let _ = jita_h
        .spawn_ball_with_combat(
            Ball {
                id: rifter_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 2.5,
                mass: 1_067_000.0,
                radius: 38.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;

    let agency_rep = ctx
        .dispatch_rpc(&mut sess, "custom_agency_provider", None, "get_content_data", &[], &kw, None)
        .await
        .expect("custom_agency_provider.get_content_data");
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSignatures", &[], &kw, None)
        .await
        .expect("scanMgr.GetSignatures");
    let PyRep::List(pieces) = agency_rep else { panic!() };
    let dsg_did = pieces
        .iter()
        .find(|p| p.dict_get("contentType").and_then(|v| v.as_str()) == Some("signature"))
        .and_then(|p| p.dict_get("dungeonID").and_then(|v| v.as_i64()))
        .expect("Deadspace signature dungeonID");
    let dsg_pos = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures[&jita_sys]
            .iter()
            .find(|sig| sig.signature_id == "DSG-30000142")
            .unwrap()
            .position
    });
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
        .await
        .unwrap();
    step_until_stopped(&ctx, jita_sys, rifter_id).await;
    for _ in 0..35 {
        let _ = jita_h.step_manual(1.0).await;
    }

    let entry_gate_id = jita_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .find(|b| {
            b.id != rifter_id
                && ctx.sde.types.get(&b.type_id).is_some_and(|t| t.group_id == 366)
                && b.position.distance(DVec3::from_array(dsg_pos)) < 50_000.0
        })
        .expect("Materialized entry acceleration gate ball")
        .id;
    let room1_id = dsg_did * 100 + 1;
    let bm_r2 = 950_190_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_r2,
            BookmarkRecord {
                bookmark_id: bm_r2,
                owner_id: char_id,
                label: "Gated Room 2".into(),
                solar_system_id: jita_sys,
                x: dsg_pos[0] + 1_200_000.0,
                y: dsg_pos[1],
                z: dsg_pos[2],
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let warp_r1_args = [PyRep::String("dungeon".into()), PyRep::Integer(room1_id)];
    let err_r1 = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r1_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_r1.code, "NaturalPhenomenaDisruptingWarp");

    let warp_r2_args = [PyRep::String("bookmark".into()), PyRep::Integer(bm_r2)];
    let err_r2 = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r2_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_r2.code, "NaturalPhenomenaDisruptingWarp");

    let g1 = kv_map(
        &ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
            .await
            .expect("Enter Room 1 via acceleration gate"),
    );
    let r1_dest = DVec3::new(g1["destinationX"].as_f64().unwrap(), g1["destinationY"].as_f64().unwrap(), g1["destinationZ"].as_f64().unwrap());
    let landed_r1 = jita_h.get_ball_snapshot(rifter_id).await.unwrap().unwrap().1.position;
    assert!(landed_r1.distance(r1_dest) < 1.0);

    // Inside deadspace pocket Room 1 -> both Abyssal and random jump filaments refused with ShipInDungeon!
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_rifter)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(950_107)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );

    // Cross-pocket warp from Room 1 to Room 2 of the same deadspace complex -> NaturalPhenomenaDisruptingWarp!
    let err_cross = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r2_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_cross.code, "NaturalPhenomenaDisruptingWarp");

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleNotAllowedInDeadspace"
    );

    let PyRep::List(r1_npcs) = &g1["npcBallIDs"] else { panic!() };
    for nid in r1_npcs {
        let npc_bid = nid.as_i64().unwrap();
        let npc_pos = jita_h.get_ball_snapshot(npc_bid).await.unwrap().unwrap().1.position;
        let _ = jita_h.set_ball_position(rifter_id, npc_pos - DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_bid)], &kw, None).await.unwrap();
        for _ in 0..30 {
            if jita_h.get_target_locks(rifter_id).await.unwrap().iter().any(|l| l.target_id == npc_bid && l.locked) {
                break;
            }
            let _ = jita_h.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(950_105), PyRep::Integer(npc_bid)], &kw, None).await.unwrap();
        for _ in 0..60 {
            if jita_h.get_ball_snapshot(npc_bid).await.unwrap().is_none() {
                break;
            }
            let _ = jita_h.step_manual(1.0).await;
        }
        assert!(jita_h.get_ball_snapshot(npc_bid).await.unwrap().is_none());
    }
    let r1_gate_id = g1["nextGateBallID"].as_i64().unwrap();
    let r1_gate_pos = jita_h.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = jita_h.set_ball_position(rifter_id, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Enter gated Room 2");

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleNotAllowedInDeadspace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(rifter_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner allowed in MWD-restricted deadspace pocket");
}
