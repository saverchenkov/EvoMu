//! Unit and integration tests for `pve_dungeon_runtime_rpc`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::on_dungeon_npc_killed;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn find_difficulty1_and_any_dungeon(sde: &SdeRegistry) -> (u32, u32) {
    let d1 = sde
        .dungeons
        .values()
        .find(|d| d.difficulty == Some(1))
        .map(|d| d.dungeon_id)
        .unwrap_or(2001);
    let any_d = sde.dungeons.keys().copied().min().unwrap_or(1);
    (d1, any_d)
}

fn make_ctx_and_ship(sys_id: SolarSystemId, char_id: i64, ship_id: i64, mass: f64) -> (ServerContext, SessionState) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("gate_runner", "pw", char_id, "GateRunner");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_200_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let rt = tokio::runtime::Handle::current();
    let handle_clone = sys_handle.clone();
    std::thread::spawn(move || {
        rt.block_on(async move {
            let _ = handle_clone.spawn_ball(ball).await;
        });
    })
    .join()
    .expect("spawn ship ball");

    let mut session = SessionState::new(301);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    (ctx, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_dungeon_spawn_gates_approach_loot_and_mwd_warp_rules() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_001;
    let ship_id: i64 = 1_030_000_001;
    let (ctx, mut session) = make_ctx_and_ship(sys_id, char_id, ship_id, 1_200_000.0);
    let (diff1_did, _) = find_difficulty1_and_any_dungeon(&ctx.sde);

    let err_no_sys = spawn_dungeon_instance(&ctx, char_id, None, &[], &HashMap::new())
        .await
        .expect_err("missing solar system");
    assert_eq!(err_no_sys.code, "MustBeInSpace");

    assert_eq!(
        get_dungeon_instance_state(&ctx, 777_666)
            .expect_err("unknown inst")
            .code,
        "DungeonInstanceNotFound"
    );

    let base_pos = [10_000_000.0, 0.0, 0.0];
    let rooms_py = PyRep::List(vec![
        PyRep::keyval([
            ("roomName", PyRep::String("Entry Chamber".into())),
            ("gateLocked", PyRep::Bool(true)),
            (
                "objects",
                PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(23)),
                    ("name", PyRep::String("Beacon Structure".into())),
                    ("offset", PyRep::List(vec![PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
                ])]),
            ),
            (
                "npcGroups",
                PyRep::List(vec![
                    PyRep::keyval([
                        ("groupID", PyRep::Integer(1)),
                        ("wave", PyRep::Integer(1)),
                        ("npcs", PyRep::List(vec![PyRep::List(vec![PyRep::Integer(48_090), PyRep::Integer(1)])])),
                    ]),
                    PyRep::keyval([
                        ("groupID", PyRep::Integer(2)),
                        ("wave", PyRep::Integer(2)),
                        ("spawnImmediately", PyRep::Bool(false)),
                        ("npcs", PyRep::List(vec![PyRep::List(vec![PyRep::Integer(48_090), PyRep::Integer(1)])])),
                    ]),
                ]),
            ),
            (
                "triggers",
                PyRep::List(vec![
                    PyRep::keyval([
                        ("kind", PyRep::String("approach".into())),
                        ("targetObjectIndex", PyRep::Integer(0)),
                        ("radius", PyRep::Real(5_000.0)),
                        ("action", PyRep::String("SpawnNpcGroup".into())),
                        ("actionGroup", PyRep::Integer(2)),
                    ]),
                    PyRep::keyval([
                        ("kind", PyRep::String("kill".into())),
                        ("targetGroup", PyRep::Integer(1)),
                        ("action", PyRep::String("UnlockGate".into())),
                    ]),
                ]),
            ),
        ]),
        PyRep::keyval([
            ("roomName", PyRep::String("Loot Vault".into())),
            ("disallowMWD", PyRep::Bool(true)),
            (
                "objects",
                PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(23)),
                    ("isContainer", PyRep::Bool(true)),
                    ("lootItems", PyRep::List(vec![PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(100)])])),
                ])]),
            ),
            (
                "triggers",
                PyRep::List(vec![PyRep::keyval([
                    ("kind", PyRep::String("loot".into())),
                    ("targetObjectIndex", PyRep::Integer(0)),
                    ("itemTypeID", PyRep::Integer(34)),
                    ("action", PyRep::String("CompleteDungeon".into())),
                ])]),
            ),
        ]),
        PyRep::keyval([("roomName", PyRep::String("Deep Chamber".into()))]),
    ]);

    let mut kwargs = HashMap::new();
    kwargs.insert("solarSystemID".into(), PyRep::Integer(i64::from(sys_id)));
    kwargs.insert("dungeonID".into(), PyRep::Integer(i64::from(diff1_did)));
    kwargs.insert("dungeonName".into(), PyRep::String("Custom Vault".into()));
    kwargs.insert("hasEntryGate".into(), PyRep::Bool(true));
    kwargs.insert(
        "position".into(),
        PyRep::List(vec![PyRep::Real(base_pos[0]), PyRep::Real(base_pos[1]), PyRep::Real(base_pos[2])]),
    );
    kwargs.insert("rooms".into(), rooms_py);

    let spawned_kv = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs)
        .await
        .expect("spawn custom dungeon");
    let inst_id = spawned_kv.dict_get("instanceID").and_then(|v| v.as_i64()).expect("instanceID");
    let entry_gate_id = spawned_kv
        .dict_get("entryGateBallID")
        .and_then(|v| v.as_i64())
        .expect("entryGateBallID");
    assert!(get_dungeon_instance_state(&ctx, inst_id).is_ok());

    let warp_err = check_warp_target_blocked_by_gated_dungeon_room(
        &ctx,
        sys_id,
        DVec3::new(base_pos[0] + 500_000.0, 0.0, 0.0),
        None,
    )
    .expect_err("room 2 warp blocked");
    assert_eq!(warp_err.code, "WarpBlockedByDeadspacePocket");

    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(88_888_888)])
        .await
        .is_none());

    let range_err = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(entry_gate_id)])
        .await
        .expect("handled")
        .expect_err("too far from gate");
    assert_eq!(range_err.code, "OutsideGateActivationRange");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let entry_pos = [base_pos[0] - 25_000.0, 0.0, 0.0];
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(entry_pos)).await;

    session.station_id = Some(60_003_760);
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(entry_gate_id)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    session.station_id = None;

    session.ship_id = None;
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(entry_gate_id)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    session.ship_id = Some(ship_id);

    let gate1_kv = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(entry_gate_id)])
        .await
        .expect("handled")
        .expect("entry gate activated");
    let r1_gate_id = gate1_kv.dict_get("nextGateBallID").and_then(|v| v.as_i64()).expect("room 1 gate id");
    assert_eq!(gate1_kv.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(1));

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(base_pos[0] + 2_000.0, 0.0, 0.0)).await;
    let approach_fired = check_dungeon_triggers(&ctx, char_id, sys_id, ship_id).await;
    assert_eq!(approach_fired, 1);

    let r1_gate_pos = DVec3::new(base_pos[0] + 5_000.0, 0.0, 0.0);
    let _ = sys_handle.set_ball_position(ship_id, r1_gate_pos).await;
    let locked_err = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(r1_gate_id)])
        .await
        .expect("handled")
        .expect_err("gate locked until group 1 killed");
    assert_eq!(locked_err.code, "GateLocked");

    let w1_npc = with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get(&inst_id).unwrap().rooms[0].npc_groups[0].npc_ball_ids[0]
    });
    on_dungeon_npc_killed(&ctx, sys_id, w1_npc, char_id).await;

    let gate2_kv = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(r1_gate_id)])
        .await
        .expect("handled")
        .expect("room 1 -> room 2 transition");
    assert_eq!(gate2_kv.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(gate2_kv.dict_get("disallowMWD").and_then(|v| v.as_bool()), Some(true));

    let mwd_err = check_deadspace_mwd_disallowed(&ctx, &sys_handle, sys_id, ship_id)
        .await
        .expect_err("MWD disallowed in Room 2");
    assert_eq!(mwd_err.code, "MicroWarpdriveDisallowedInDeadspace");

    let r2_cont_id = with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get(&inst_id).unwrap().rooms[1].objects[0].ball_id
    });
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), r2_cont_id, 34).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get(&inst_id).unwrap();
        assert!(inst.completed);
        assert!(inst.despawned);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sde_dungeon_and_operation_registration_and_hull_restriction() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_002;
    let ship_id: i64 = 1_030_000_002;
    let (ctx, mut session) = make_ctx_and_ship(sys_id, char_id, ship_id, 50_000_000.0);
    let (diff1_did, any_did) = find_difficulty1_and_any_dungeon(&ctx.sde);

    let op_id1 = register_sde_operation_dungeon_instance(&ctx, char_id, sys_id, any_did);
    let op_id2 = register_sde_operation_dungeon_instance(&ctx, char_id, sys_id, any_did);
    assert_eq!(op_id1, op_id2);

    let sde_id1 = spawn_sde_dungeon_runtime_instance(
        &ctx, char_id, sys_id, diff1_did, "Deadspace", [25_000_000.0, 0.0, 0.0], false,
    )
    .await
    .expect("spawn sde dungeon");
    let sde_id2 = spawn_sde_dungeon_runtime_instance(
        &ctx, char_id, sys_id, diff1_did, "Deadspace", [25_000_000.0, 0.0, 0.0], false,
    )
    .await
    .expect("deduplicate sde dungeon");
    assert_eq!(sde_id1, sde_id2);

    let gate_pos = DVec3::new(25_005_000.0, 0.0, 0.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(ship_id, gate_pos).await;
    let dng_tag = PyRep::String(format!("DNG-INST-{sde_id1}"));
    let locked_err = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[dng_tag.clone()])
        .await
        .expect("handled")
        .expect_err("locked gate rejected");
    assert_eq!(locked_err.code, "GateLocked");

    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        if let Some(inst) = store.instances.get_mut(&sde_id1) {
            for r in &mut inst.rooms {
                r.gate_locked = false;
            }
        }
    });
    let hull_err = try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[dng_tag])
        .await
        .expect("handled")
        .expect_err("oversized hull rejected");
    assert_eq!(hull_err.code, "ShipRestrictedByDungeonGate");
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_rpc_extra_unit_tests.rs"]
mod pve_dungeon_runtime_rpc_extra_unit_tests;
