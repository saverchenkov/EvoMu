//! Unit tests for `gm_soak_commands` (`#1088`).

use super::*;
use eve_db::WorldRepository;
use eve_sde::{InvType, RaceInfo, SdeRegistry, TypeMaterialEntry};
use std::sync::Arc;

fn make_test_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    mass: f64,
    volume: f64,
    capacity: f64,
    radius: f64,
    base_price: f64,
    race_id: Option<u32>,
    faction_id: Option<u32>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass,
        volume,
        capacity,
        radius,
        base_price,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id,
        faction_id,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: Some(0),
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_test_ctx() -> (ServerContext, i64) {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.entry(29_768).or_insert_with(|| {
        make_test_inv_type(
            29_768,
            673,
            11,
            "Independent Osprey",
            11_000_000.0,
            100_000.0,
            400.0,
            120.0,
            0.0,
            Some(1),
            Some(500_021),
        )
    });
    sde.types.entry(587).or_insert_with(|| {
        make_test_inv_type(
            587,
            25,
            6,
            "Rifter",
            1_067_000.0,
            27_289.0,
            135.0,
            42.0,
            0.0,
            Some(2),
            None,
        )
    });
    sde.types.entry(34).or_insert_with(|| {
        make_test_inv_type(
            34,
            18,
            4,
            "Tritanium",
            0.0,
            0.01,
            0.0,
            0.0,
            5.0,
            None,
            None,
        )
    });
    sde.type_materials.insert(
        587,
        vec![TypeMaterialEntry {
            material_type_id: 34,
            quantity: 20_000,
        }],
    );
    sde.races.entry(1).or_insert(RaceInfo {
        race_id: 1,
        name: "Caldari".into(),
        corvette_type_id: Some(601),
        starter_skills: HashMap::from([(3300, 3), (3327, 2)]),
        description: String::new(),
    });
    let repo = Arc::new(WorldRepository::with_demo_seed());
    let cid = 90_000_001_i64;
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.race_id = 1;
        }
    });
    (ServerContext::with_sde_and_repository(Arc::new(sde), repo), cid)
}

#[test]
fn test_deterministic_soak_seed_ring_and_deadwarp_anchor() {
    let s1 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 5);
    let s2 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 5);
    let s3 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 6);
    assert_eq!(s1, s2);
    assert_ne!(s1, s3);

    let center = DVec3::new(1_000.0, 2_000.0, 3_000.0);
    let ring_a = ring_positions_around(center, 4, SOAK_LOGI_ORBIT_RANGE_M, s1);
    let ring_b = ring_positions_around(center, 4, SOAK_LOGI_ORBIT_RANGE_M, s1);
    assert_eq!(ring_a, ring_b);
    assert_eq!(ring_a.len(), 4);
    for p in &ring_a {
        let horiz = DVec3::new(p.x - center.x, 0.0, p.z - center.z).length();
        assert!((horiz - SOAK_LOGI_ORBIT_RANGE_M).abs() < 1e-3);
        assert!((p.y - center.y).abs() <= 500.0);
    }

    let anchor_a = deterministic_deadwarp_anchor(center, 30_000_142, 100_001);
    let anchor_b = deterministic_deadwarp_anchor(center, 30_000_142, 100_001);
    assert_eq!(anchor_a, anchor_b);
    let dist_au = anchor_a.distance(center) / crate::services::space_destiny::AU_IN_METERS;
    assert!((dist_au - DEADWARP_DISTANCE_AU).abs() < 1e-6);
}

#[test]
fn test_derive_sde_wreck_loot_is_deterministic_and_uses_sde_materials() {
    let (ctx, _) = make_test_ctx();
    let loot_a = derive_sde_wreck_loot(&ctx.sde, 587, DEFAULT_WRECK_TYPE_ID, 42);
    let loot_b = derive_sde_wreck_loot(&ctx.sde, 587, DEFAULT_WRECK_TYPE_ID, 42);
    assert_eq!(loot_a, loot_b);
    assert!(
        loot_a.iter().any(|&(tid, qty)| tid == 34 && qty >= 1),
        "expected Tritanium derived from SDE type_materials: {loot_a:?}"
    );
}

#[tokio::test]
async fn test_gm_soak_argument_validation_and_session_dump() {
    let (ctx, cid) = make_test_ctx();
    let mut sess = SessionState::new(777_001);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);

    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak logi 0").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak logi 99").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/deadwarp bogus").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/sysjunkclear extra").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/spawnwreck").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/spawnwreck 99999999").await,
        Some(Err(e)) if e.code == "TypeNotFound"
    ));
    assert!(matches!(
        handle_session_cmd(&ctx, &sess, cid, "/session status"),
        Err(e) if e.code == "InvalidArguments"
    ));

    let dump = handle_session_cmd(&ctx, &sess, cid, "/session").expect("/session dump");
    assert!(dump.dict_get("sessionDict").is_some());
    assert!(dump.dict_get("wireState").is_some());
    assert!(dump.dict_get("entries").is_some());
    assert!(dump.dict_get("destinyStamps").is_some());

    let back = handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/backintime")
        .await
        .expect("matched")
        .expect("/backintime");
    assert_eq!(back.dict_get("queueLength").and_then(|v| v.as_i64()), Some(0));
    ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&cid).expect("starter skills");
        let race_starter = &ctx.sde.races.get(&1).expect("Caldari race").starter_skills;
        let expected_count = race_starter.values().filter(|&&lvl| lvl > 0).count();
        assert!(expected_count > 0);
        assert_eq!(sk.len(), expected_count);
        for (&tid, &lvl) in race_starter {
            if lvl > 0 {
                assert_eq!(sk.get(&tid).map(|s| s.level), Some(lvl));
            }
        }
        assert!(snap.skill_queues.get(&cid).is_none_or(|q| q.is_empty()));
    });
}

fn make_test_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn unwrap_destiny_action(entry: &PyRep) -> &PyRep {
    let PyRep::Tuple(stamp_and_action) = entry else {
        panic!("expected (stamp, action) tuple, got {entry:?}");
    };
    assert_eq!(stamp_and_action.len(), 2);
    &stamp_and_action[1]
}

fn assert_on_special_fx_wire_tuple(
    encoded: &PyRep,
    expected_ship_id: i64,
    expected_mod_id: i64,
    expected_mod_type_id: u32,
    expected_target_id: i64,
    expected_guid: &str,
    expected_active: i64,
) {
    let PyRep::Tuple(outer) = encoded else {
        panic!("expected outer Destiny OnSpecialFX tuple, got {encoded:?}");
    };
    assert_eq!(outer.len(), 2);
    assert_eq!(outer[0].as_str(), Some("OnSpecialFX"));
    let PyRep::Tuple(args) = &outer[1] else {
        panic!("expected 14-arg OnSpecialFX tuple, got {:?}", outer[1]);
    };
    assert_eq!(args.len(), 14, "V24 OnSpecialFX requires 14 args: {args:?}");
    assert_eq!(args[0].as_i64(), Some(expected_ship_id), "shipID");
    assert_eq!(args[1].as_i64(), Some(expected_mod_id), "moduleID");
    assert_ne!(args[1].as_i64(), Some(expected_target_id), "moduleID must not equal target_ship_id");
    assert_eq!(
        args[2].as_i64(),
        Some(i64::from(expected_mod_type_id)),
        "moduleTypeID"
    );
    assert_eq!(args[3].as_i64(), Some(expected_target_id), "targetID");
    assert_eq!(args[4], PyRep::None, "otherTypeID");
    assert_eq!(args[5].as_str(), Some(expected_guid), "guid");
    assert_eq!(args[6].as_bool(), Some(false), "isOffensive");
    assert_eq!(args[7].as_i64(), Some(expected_active), "start");
    assert_eq!(args[8].as_i64(), Some(expected_active), "active");
    assert_eq!(args[9].as_f64(), Some(2_000.0), "duration");
    assert_eq!(args[10].as_i64(), Some(0), "repeat");
}

#[test]
fn test_logi_soak_fx_helpers_and_destiny_wire_encoding() {
    let (ctx, _) = make_test_ctx();
    let shield_tid = pick_logi_soak_module_type_id(&ctx.sde, "effects.ShieldTransfer");
    let armor_tid = pick_logi_soak_module_type_id(&ctx.sde, "effects.RemoteArmourRepair");
    assert_eq!(shield_tid, DEFAULT_LOGI_SOAK_SHIELD_MOD_TYPE_ID);
    assert_eq!(armor_tid, DEFAULT_LOGI_SOAK_ARMOR_MOD_TYPE_ID);
    assert_eq!(ctx.sde.types.get(&shield_tid).map(|t| t.group_id), Some(41));
    assert_eq!(ctx.sde.types.get(&armor_tid).map(|t| t.group_id), Some(325));

    let empty_sde = eve_sde::SdeRegistry::default();
    assert_eq!(
        pick_logi_soak_module_type_id(&empty_sde, "effects.ShieldTransfer"),
        DEFAULT_LOGI_SOAK_SHIELD_MOD_TYPE_ID
    );
    assert_eq!(
        pick_logi_soak_module_type_id(&empty_sde, "effects.RemoteArmourRepair"),
        DEFAULT_LOGI_SOAK_ARMOR_MOD_TYPE_ID
    );

    let npc_id = 900_000_001_i64;
    let target_ship_id = 100_000_001_i64;
    let mod_s0 = logi_soak_synthetic_module_id(npc_id, 0, 0);
    let mod_a0 = logi_soak_synthetic_module_id(npc_id, 1, 0);
    let mod_s1 = logi_soak_synthetic_module_id(npc_id, 0, 1);
    let mod_other = logi_soak_synthetic_module_id(npc_id + 1, 0, 0);
    assert_ne!(mod_s0, target_ship_id);
    assert_ne!(mod_s0, npc_id);
    assert_ne!(mod_s0, GM_LOGI_SOAK_DBUFF_SOURCE_ID);
    assert_eq!(mod_s0, logi_soak_synthetic_module_id(npc_id, 0, 0));
    let distinct: std::collections::HashSet<i64> = [mod_s0, mod_a0, mod_s1, mod_other].into();
    assert_eq!(distinct.len(), 4);

    let mut bp = eve_destiny::Ballpark::new(30_000_142);
    bp.add_ball(make_test_ball(target_ship_id, 587, DVec3::ZERO));
    bp.add_ball(make_test_ball(npc_id, 29_768, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.pending_events.clear();

    let mod_type_ids = logi_soak_fx_module_type_ids(&ctx.sde);
    emit_logi_soak_fx_for_npc(&mut bp, mod_type_ids, npc_id, target_ship_id, 2_000.0, 0, true);
    assert_eq!(bp.pending_events.len(), 2);

    for (idx, (guid, expected_tid, expected_mod)) in [
        ("effects.ShieldTransfer", shield_tid, mod_s0),
        ("effects.RemoteArmourRepair", armor_tid, mod_a0),
    ]
    .into_iter()
    .enumerate()
    {
        let encoded = eve_destiny::wire::encode_destiny_event(10, &bp.pending_events[idx], Some(&bp));
        assert_on_special_fx_wire_tuple(
            unwrap_destiny_action(&encoded),
            npc_id,
            expected_mod,
            expected_tid,
            target_ship_id,
            guid,
            1,
        );
    }

    bp.pending_events.clear();
    emit_logi_soak_fx_for_npc(&mut bp, mod_type_ids, npc_id, target_ship_id, 2_000.0, 0, false);
    assert_eq!(bp.pending_events.len(), 2);
    for (idx, (guid, expected_tid, expected_mod)) in [
        ("effects.ShieldTransfer", shield_tid, mod_s0),
        ("effects.RemoteArmourRepair", armor_tid, mod_a0),
    ]
    .into_iter()
    .enumerate()
    {
        let encoded = eve_destiny::wire::encode_destiny_event(11, &bp.pending_events[idx], Some(&bp));
        assert_on_special_fx_wire_tuple(
            unwrap_destiny_action(&encoded),
            npc_id,
            expected_mod,
            expected_tid,
            target_ship_id,
            guid,
            0,
        );
    }

    assert!(bp.remove_ball(npc_id).is_some());
    assert!(bp.special_fx_meta.is_empty());
}

#[tokio::test]
async fn test_stop_logi_soak_flushes_stop_fx_before_remove_ball() {
    let (ctx, cid) = make_test_ctx();
    let sys_id = 30_000_142_u32;
    let target_ship_id = 100_000_055_i64;
    let npc_id = 900_000_055_i64;

    let sys = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys.ballpark.write().expect("ballpark write");
        bp.add_ball(make_test_ball(target_ship_id, 587, DVec3::ZERO));
        bp.add_ball(make_test_ball(npc_id, 29_768, DVec3::new(12_000.0, 0.0, 0.0)));
        bp.register_observer(target_ship_id);
        let _ = bp.drain_observer_events(target_ship_id);
    }
    let mut rx = sys.subscribe();

    with_mut_combat_runtime(&ctx, |rt| {
        rt.gm_logi_soaks.insert(
            sys_id,
            GmLogiSoakState {
                solar_system_id: sys_id,
                target_char_id: cid,
                target_ship_id,
                npc_type_id: 29_768,
                npc_ids: vec![npc_id],
                seed: 1,
                cycle_count: 1,
                cycle_interval_ticks: SOAK_LOGI_CYCLE_INTERVAL_TICKS,
                last_cycle_tick: 5,
                started_unix: 100,
                last_cycle_unix: 102,
                total_shield_repaired: 450.0,
                total_armor_repaired: 450.0,
            },
        );
    });

    let stopped = stop_logi_soak_in_system(&ctx, sys_id)
        .await
        .expect("stopped logi soak");
    assert_eq!(stopped.npc_ids, vec![npc_id]);

    let first_bcast = rx.try_recv().expect("expected flushed stop-FX TickBroadcast");
    let updates = first_bcast
        .observer_updates_pyrep
        .get(&target_ship_id)
        .expect("target observer stop FX updates");
    let PyRep::List(entries) = updates else {
        panic!("expected DoDestinyUpdate list, got {updates:?}");
    };
    assert_eq!(entries.len(), 2, "expected ShieldTransfer + RemoteArmourRepair stop FX: {entries:?}");

    let shield_mod = logi_soak_synthetic_module_id(npc_id, 0, 0);
    let armor_mod = logi_soak_synthetic_module_id(npc_id, 1, 0);
    let mut seen_guids = Vec::new();
    for entry in entries {
        let action = unwrap_destiny_action(entry);
        let PyRep::Tuple(outer) = action else {
            panic!("expected action tuple: {action:?}");
        };
        if outer[0].as_str() == Some("OnSpecialFX") {
            let PyRep::Tuple(args) = &outer[1] else {
                panic!("expected 14-arg tuple");
            };
            let guid = args[5].as_str().expect("guid").to_string();
            let (expected_mod, expected_tid) = if guid == "effects.ShieldTransfer" {
                (shield_mod, DEFAULT_LOGI_SOAK_SHIELD_MOD_TYPE_ID)
            } else {
                (armor_mod, DEFAULT_LOGI_SOAK_ARMOR_MOD_TYPE_ID)
            };
            assert_on_special_fx_wire_tuple(
                action,
                npc_id,
                expected_mod,
                expected_tid,
                target_ship_id,
                &guid,
                0,
            );
            seen_guids.push(guid);
        }
    }
    assert_eq!(
        seen_guids,
        vec![
            "effects.ShieldTransfer".to_string(),
            "effects.RemoteArmourRepair".to_string()
        ]
    );

    let bp = sys.ballpark.read().expect("ballpark read after stop");
    assert!(!bp.balls.contains_key(&npc_id));
    assert!(bp.special_fx_meta.is_empty());
}

#[tokio::test]
async fn test_session_dump_rpc_history_offsets_unaffected_by_virtual_clock_advance() {
    let (ctx, cid) = make_test_ctx();
    let mut sess = SessionState::new(777_002);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);

    ctx.flight_recorder.record(
        sess.session_id,
        "charMgr",
        None,
        "GetPublicInfo",
        &[],
        &HashMap::new(),
        std::time::Duration::from_micros(250),
        "ok",
    );

    // Advance the virtual server clock by 24 hours; FlightRecorder timestamps and `/session`
    // relative `offsetMs` remain in the same host wall clock domain.
    ctx.advance_secs(86_400);

    let dump = handle_session_cmd(&ctx, &sess, cid, "/session").expect("/session dump");
    assert_eq!(
        dump.dict_get("rpcHistoryCount").and_then(|v| v.as_i64()),
        Some(1)
    );
    let Some(PyRep::List(hist)) = dump.dict_get("rpcHistory") else {
        panic!("expected rpcHistory list");
    };
    assert_eq!(hist.len(), 1);
    let offset_ms = hist[0]
        .dict_get("offsetMs")
        .and_then(|v| v.as_i64())
        .expect("offsetMs integer");
    assert!(
        (-60_000..=0).contains(&offset_ms),
        "expected host-wall relative offsetMs in [-60_000, 0], got {offset_ms}"
    );
}

#[tokio::test]
async fn bf01_gm_soak_commands_reject_active_upwell_structure_control() {
    let (ctx, cid) = make_test_ctx();
    let st_id = 1_000_000_009_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = Some(st_id);
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = st_id;
        }
    });
    let mut sess = SessionState::new(777_003);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);
    sess.structure_id = Some(st_id);
    sess.ship_id = Some(st_id);

    for cmd in ["/deadwarp", "/soak logi 1", "/spawnwreck 587"] {
        let err = handle_gm_soak_slash_cmd(&ctx, &sess, cid, cmd)
            .await
            .expect("matched")
            .unwrap_err();
        assert_eq!(err.code, "MustBeInSpace", "{cmd}");
    }
}
