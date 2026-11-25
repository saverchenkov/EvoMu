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
    ctx.db
        .with_mut_snapshot(|snap| snap.items.retain(|_, item| item.location_id != r2_cont_id));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_dungeon_approach_is_read_pure_until_trigger_fires_and_allocates_monotonic_instance_ids() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_099;
    let ship_id: i64 = 1_030_000_099;
    let (ctx, _session) = make_ctx_and_ship(sys_id, char_id, ship_id, 1_200_000.0);
    let (diff1_did, _) = find_difficulty1_and_any_dungeon(&ctx.sde);

    // PVE-212: movement ticks with zero active dungeons do not open a write tx.
    let commits_idle = ctx.db.tx_commit_count();
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    assert_eq!(ctx.db.tx_commit_count(), commits_idle);

    let rooms_py = PyRep::List(vec![PyRep::keyval([
        ("roomName", PyRep::String("Approach Gate Chamber".into())),
        (
            "objects",
            PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(23)),
                ("name", PyRep::String("Sensor Array".into())),
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
            PyRep::List(vec![PyRep::keyval([
                ("kind", PyRep::String("approach".into())),
                ("targetObjectIndex", PyRep::Integer(0)),
                ("radius", PyRep::Real(5_000.0)),
                ("action", PyRep::String("SpawnNpcGroup".into())),
                ("actionGroup", PyRep::Integer(2)),
            ])]),
        ),
    ])]);

    let mut kwargs = HashMap::new();
    kwargs.insert("solarSystemID".into(), PyRep::Integer(i64::from(sys_id)));
    kwargs.insert("dungeonID".into(), PyRep::Integer(i64::from(diff1_did)));
    kwargs.insert("dungeonName".into(), PyRep::String("Approach Fast-Path".into()));
    kwargs.insert("hasEntryGate".into(), PyRep::Bool(false));
    kwargs.insert(
        "position".into(),
        PyRep::List(vec![PyRep::Real(100_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    );
    kwargs.insert("rooms".into(), rooms_py);

    let spawned_1 = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs)
        .await
        .expect("spawn first dungeon");
    let inst_id_1 = spawned_1
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("instanceID 1");

    // PVE-212: ship at DVec3::ZERO is 102 km from beacon (radius 5 km) -> zero DB commits.
    let commits_out_of_range = ctx.db.tx_commit_count();
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    assert_eq!(ctx.db.tx_commit_count(), commits_out_of_range);

    // Move within 5 km of Beacon at [102_000, 0, 0] -> trigger fires and commits transition.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(102_000.0, 0.0, 0.0))
        .await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    let commits_after_fire = ctx.db.tx_commit_count();
    assert!(
        commits_after_fire > commits_out_of_range,
        "firing approach trigger should persist runtime and wave state"
    );

    // Repeated approach tick after trigger has fired is again read-pure.
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    assert_eq!(ctx.db.tx_commit_count(), commits_after_fire);

    // PVE-213: second spawn allocates monotonic unique instanceID and distinct entity IDs.
    let spawned_2 = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs)
        .await
        .expect("spawn second dungeon");
    let inst_id_2 = spawned_2
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("instanceID 2");
    assert!(inst_id_1 > 0 && inst_id_2 > inst_id_1);

    let (ids_1, ids_2) = crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        let i1 = store.instances.get(&inst_id_1).expect("inst 1");
        let i2 = store.instances.get(&inst_id_2).expect("inst 2");
        (
            (i1.rooms[0].objects[0].ball_id, i1.rooms[0].npc_groups[0].npc_ball_ids[0]),
            (i2.rooms[0].objects[0].ball_id, i2.rooms[0].npc_groups[0].npc_ball_ids[0]),
        )
    });
    assert!(ids_1.0 > 0 && ids_1.1 > 0 && ids_2.0 > 0 && ids_2.1 > 0);
    assert_ne!(ids_1.0, ids_2.0);
    assert_ne!(ids_1.1, ids_2.1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_indexed_multi_room_dng_inst_traversal_and_enter_next_room_observatory_guard() {
    use crate::services::pve_dungeon_runtime::with_dungeon_runtime_store;
    use crate::services::pve_exploration::handle_pve_exploration;
    use crate::services::pve_fob_observatory::{get_observatory_site_state, spawn_observatory_flashpoint};

    let pochven_sys = shared_sde()
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE has Pochven solar system");
    let char_id: i64 = 90_040_015;
    let ship_id: i64 = 1_020_000_005;
    let (ctx, mut sess) = make_ctx_and_ship(pochven_sys, char_id, ship_id, 1_000_000.0);
    let _ = spawn_observatory_flashpoint(&ctx, pochven_sys)
        .await
        .expect("spawn Observatory Flashpoint");
    let obs = get_observatory_site_state(&ctx, pochven_sys).expect("observatory persisted");
    let sys_handle = ctx.systems.get_or_spawn(pochven_sys);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(obs.gate_pos)).await;
    let world_ark_before = sys_handle
        .get_ball_snapshot(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark spawned");
    let kw = HashMap::new();

    // R4-P1-02: keeper.EnterNextDungeonRoom(obs.gate_id) must route through Observatory guards
    // (rejecting solo pilot) and must never invoke generic runtime gate activation or reconcile overwrite.
    let enter_obs_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "EnterNextDungeonRoom",
        &[PyRep::Integer(obs.gate_id)],
        &kw,
    )
    .await
    .expect("Observatory gate handled")
    .expect_err("solo pilot must be rejected by Observatory fleet guard");
    assert_ne!(enter_obs_err.code, "DungeonNotFound");
    assert!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &[PyRep::Integer(obs.gate_id)])
            .await
            .is_none(),
        "Flashpoint integer gate_id must be excluded from generic dungeon runtime gate activation"
    );
    let (_, ship_after_obs) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ship_after_obs.position.distance(DVec3::from_array(obs.gate_pos)) < 1.0);
    let world_ark_after = sys_handle
        .get_ball_snapshot(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark still present");
    assert_eq!(world_ark_after.1.radius, world_ark_before.1.radius);

    // R4-P1-01 (Part A): 3-room 0-indexed (0, 1, 2) traversal without entry gate: Room 0 -> Room 1 -> Room 2.
    let (diff1_did, _) = find_difficulty1_and_any_dungeon(&ctx.sde);
    let (r0_pos, r1_pos, r2_pos) = (
        [85_000_000.0, 0.0, 0.0],
        [85_500_000.0, 0.0, 0.0],
        [86_000_000.0, 0.0, 0.0],
    );
    let (g0_id, g1_id) = (77_990_001_i64, 77_990_002_i64);
    let mk_room = |idx: usize, pos: [f64; 3], dest: [f64; 3], gid: Option<i64>| DungeonRuntimeRoom {
        room_index: idx,
        room_id: 9780 + (idx as i64),
        room_name: format!("Z{idx}"),
        position: pos,
        gate_destination_pos: dest,
        gated: idx > 0,
        gate_ball_id: gid,
        gate_locked: false,
        ..Default::default()
    };
    let inst_no_entry = DungeonRuntimeInstance {
        instance_id: 978_001,
        dungeon_id: diff1_did,
        dungeon_name: "ZeroIndexed".into(),
        category: "Deadspace".into(),
        archetype: "Deadspace".into(),
        solar_system_id: pochven_sys,
        owner_char_id: char_id,
        base_position: r0_pos,
        current_room_index: 0,
        rooms: vec![
            mk_room(0, r0_pos, r0_pos, Some(g0_id)),
            mk_room(1, r1_pos, [r1_pos[0] - 5_000.0, 0.0, 0.0], Some(g1_id)),
            mk_room(2, r2_pos, [r2_pos[0] - 5_000.0, 0.0, 0.0], None),
        ],
        created_unix: 1_775_300_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(978_001, inst_no_entry);
    });
    let tag_a = [PyRep::String("DNG-INST-978001".into())];
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r0_pos[0] + 5_000.0, 0.0, 0.0)).await;
    let step1 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_a)
        .await
        .expect("R0->R1")
        .expect("R0 gate succeeds");
    assert_eq!(step1.dict_get("roomID").and_then(|v| v.as_i64()), Some(9781));

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 5_000.0, 0.0, 0.0)).await;
    let step2 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_a)
        .await
        .expect("R1->R2")
        .expect("R1 gate must resolve rooms[1], not loop back to rooms[0]");
    assert_eq!(step2.dict_get("roomID").and_then(|v| v.as_i64()), Some(9782));
    assert!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_a)
            .await
            .is_none(),
        "final room must not expose another gate"
    );

    // R4-P1-01 (Part B): entry-gated 0-indexed (0, 1, 2) must keep entry->Room 0 unambiguous from rooms[1].room_index == 1.
    let eg_pos = [89_900_000.0, 0.0, 0.0];
    let eg_id = 77_990_010_i64;
    let mut r0 = mk_room(0, [90_000_000.0, 0.0, 0.0], [90_000_000.0, 0.0, 0.0], Some(77_990_011));
    r0.gated = true;
    r0.disallow_mwd = Some(true);
    let inst_with_entry = DungeonRuntimeInstance {
        instance_id: 978_002,
        dungeon_id: diff1_did,
        dungeon_name: "ZeroIndexedEntry".into(),
        category: "Deadspace".into(),
        archetype: "Deadspace".into(),
        solar_system_id: pochven_sys,
        owner_char_id: char_id,
        base_position: eg_pos,
        entry_gate_pos: Some(eg_pos),
        entry_gate_ball_id: Some(eg_id),
        current_room_index: 0,
        rooms: vec![
            r0,
            mk_room(1, [90_500_000.0, 0.0, 0.0], [90_495_000.0, 0.0, 0.0], Some(77_990_012)),
            mk_room(2, [91_000_000.0, 0.0, 0.0], [90_995_000.0, 0.0, 0.0], None),
        ],
        created_unix: 1_775_300_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(978_002, inst_with_entry);
    });
    let tag_b = [PyRep::String("DNG-INST-978002".into())];
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(eg_pos)).await;
    let e0 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_b).await.unwrap().unwrap();
    assert_eq!(e0.dict_get("roomID").and_then(|v| v.as_i64()), Some(9780));
    assert!(is_ship_in_mwd_disallowed_dungeon_pocket(&ctx, pochven_sys, DVec3::new(90_000_000.0, 0.0, 0.0)));
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(90_005_000.0, 0.0, 0.0)).await;
    let e1 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_b).await.unwrap().unwrap();
    assert_eq!(e1.dict_get("roomID").and_then(|v| v.as_i64()), Some(9781));
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(90_505_000.0, 0.0, 0.0)).await;
    let e2 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &tag_b).await.unwrap().unwrap();
    assert_eq!(e2.dict_get("roomID").and_then(|v| v.as_i64()), Some(9782));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flashpoint_warp_landing_and_reconcile_preserve_world_ark_hp_locks_and_dead_defenders() {
    use crate::services::pve_dungeon_runtime::{
        on_warp_landed_at_dungeon_pocket_if_present, reconcile_and_rematerialize_dungeon_instance,
    };
    use crate::services::pve_fob_observatory::{get_observatory_site_state, spawn_observatory_flashpoint};
    use eve_destiny::TargetLock;

    let pochven_sys = shared_sde()
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE has Pochven solar system");
    let char_id: i64 = 90_040_016;
    let ship_id: i64 = 1_020_000_006;
    let (ctx, _sess) = make_ctx_and_ship(pochven_sys, char_id, ship_id, 1_000_000.0);
    let _ = spawn_observatory_flashpoint(&ctx, pochven_sys)
        .await
        .expect("spawn Observatory Flashpoint");
    let obs = get_observatory_site_state(&ctx, pochven_sys).expect("observatory persisted");
    let sys_handle = ctx.systems.get_or_spawn(pochven_sys);

    let (_, ark_before) = sys_handle
        .get_ball_snapshot(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark ball spawned");
    let ark_cs_before = sys_handle
        .get_combat_state(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark CombatState spawned");
    assert_eq!(ark_cs_before.max_shield_hp, 200_000.0);
    assert_eq!(ark_cs_before.max_armor_hp, 400_000.0);
    assert_eq!(ark_cs_before.max_structure_hp, 200_000.0);

    // Damage World Ark, establish a target lock, and kill one defense-wave NPC before warp landing.
    sys_handle
        .modify_combat_state(obs.world_ark_id, |cs| {
            cs.shield_hp = 150_000.0;
        })
        .await
        .expect("damage World Ark");
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: obs.world_ark_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock World Ark");
    let dead_defender = obs.defense_wave_ids[0];
    let live_defender = obs.defense_wave_ids[1];
    let def_cs_before = sys_handle
        .get_combat_state(live_defender)
        .await
        .unwrap()
        .expect("defender CombatState");
    assert!(sys_handle.remove_ball(dead_defender).await.is_some());

    // Warping to outer gate or interior pocket and invoking reconcile must leave Flashpoint untouched.
    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, pochven_sys, obs.gate_pos).await;
    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, pochven_sys, obs.site_pos).await;
    assert_eq!(
        reconcile_and_rematerialize_dungeon_instance(&ctx, pochven_sys, obs.dungeon_instance_id).await,
        0
    );

    let (_, ark_after) = sys_handle
        .get_ball_snapshot(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark still present");
    assert_eq!(ark_after.radius, ark_before.radius);
    let ark_cs_after = sys_handle
        .get_combat_state(obs.world_ark_id)
        .await
        .unwrap()
        .expect("World Ark CombatState still present");
    assert_eq!(ark_cs_after.shield_hp, 150_000.0);
    assert_eq!(ark_cs_after.max_shield_hp, 200_000.0);
    assert_eq!(ark_cs_after.max_armor_hp, 400_000.0);
    assert_eq!(ark_cs_after.max_structure_hp, 200_000.0);

    let locks = sys_handle.get_target_locks(ship_id).await.unwrap();
    assert!(locks.iter().any(|l| l.target_id == obs.world_ark_id && l.locked));
    assert!(sys_handle.get_ball_snapshot(dead_defender).await.unwrap().is_none());
    assert_eq!(
        sys_handle.get_combat_state(live_defender).await.unwrap(),
        Some(def_cs_before)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_preserves_live_custom_radius_and_bumped_dungeon_objects_while_restoring_missing() {
    use crate::services::pve_dungeon_runtime::{
        materialize_dungeon_room_entities, reconcile_and_rematerialize_dungeon_instance,
        with_dungeon_runtime_store,
    };
    use eve_db::service_domains::{DungeonNpcGroup, DungeonNpcSpawnEntry};
    use eve_destiny::{CombatState, TargetLock};

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_017;
    let ship_id: i64 = 1_020_000_007;
    let (ctx, mut sess) = make_ctx_and_ship(sys_id, char_id, ship_id, 1_000_000.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (diff1_did, _) = find_difficulty1_and_any_dungeon(&ctx.sde);
    let r0_pos = [92_000_000.0, 0.0, 0.0];
    let live_obj_id = 88_001_001_i64;
    let missing_obj_id = 88_001_002_i64;
    let gate_id = 88_001_003_i64;
    let custom_gate_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 366 && t.type_id != 17_831)
        .map(|t| t.type_id)
        .unwrap_or(17_831);

    let inst = DungeonRuntimeInstance {
        instance_id: 978_003,
        dungeon_id: diff1_did,
        dungeon_name: "ReconcileLiveObject".into(),
        category: "Deadspace".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r0_pos,
        current_room_index: 0,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 0,
                room_id: 9783,
                room_name: "R0".into(),
                position: r0_pos,
                gate_destination_pos: [92_500_000.0, 0.0, 0.0],
                spawned: true,
                entered: true,
                objects: vec![
                    DungeonRuntimeObject {
                        ball_id: live_obj_id,
                        type_id: 23,
                        name: "CustomLiveBeacon".into(),
                        position: r0_pos,
                        radius: 1_500.0,
                        spawned: true,
                        destructible: true,
                        ..Default::default()
                    },
                    DungeonRuntimeObject {
                        ball_id: missing_obj_id,
                        type_id: 23,
                        name: "MissingBeacon".into(),
                        position: [r0_pos[0] + 2_000.0, 0.0, 0.0],
                        radius: 1_500.0,
                        spawned: true,
                        destructible: false,
                        ..Default::default()
                    },
                ],
                gate_links: vec![DungeonGateLink {
                    gate_ball_id: Some(gate_id),
                    gate_position: [r0_pos[0] + 5_000.0, 0.0, 0.0],
                    target_room_index: 1,
                    destination_pos: [92_500_000.0, 0.0, 0.0],
                    gate_type_id: Some(custom_gate_type),
                    spawned: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 9784,
                room_name: "R1".into(),
                position: [92_500_000.0, 0.0, 0.0],
                gated: true,
                ..Default::default()
            },
        ],
        created_unix: 1_775_300_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(978_003, inst);
    });

    // Spawn live_obj with custom radius 220m, bumped position, damaged CombatState, and target lock.
    let bumped_pos = DVec3::new(r0_pos[0] + 120.0, 0.0, 0.0);
    let mut live_ball = Ball {
        id: live_obj_id,
        type_id: 23,
        position: bumped_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 220.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    live_ball.radius = 220.0;
    let custom_cs = CombatState {
        shield_hp: 7_777.0,
        max_shield_hp: 10_000.0,
        armor_hp: 5_000.0,
        max_armor_hp: 5_000.0,
        structure_hp: 5_000.0,
        max_structure_hp: 5_000.0,
        ..Default::default()
    };
    let _ = sys_handle.spawn_ball_with_combat(live_ball, custom_cs).await;
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: live_obj_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock live_obj");

    // Reconcile + already_spawned materialization restores missing_obj & gate, but leaves live_obj intact.
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx, sys_id, 978_003).await, 2);
    let _ = materialize_dungeon_room_entities(&ctx, sys_id, 978_003, 0).await.unwrap();
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx, sys_id, 978_003).await, 0);

    let (_, live_after) = sys_handle.get_ball_snapshot(live_obj_id).await.unwrap().unwrap();
    assert!(live_after.position.distance(bumped_pos) < 1.0);
    assert_eq!(live_after.radius, 220.0);
    let cs_after = sys_handle.get_combat_state(live_obj_id).await.unwrap().unwrap();
    assert_eq!(cs_after.shield_hp, 7_777.0);
    assert_eq!(cs_after.max_shield_hp, 10_000.0);
    assert!(sys_handle
        .get_target_locks(ship_id)
        .await
        .unwrap()
        .iter()
        .any(|l| l.target_id == live_obj_id && l.locked));

    let (_, missing_after) = sys_handle.get_ball_snapshot(missing_obj_id).await.unwrap().unwrap();
    assert_eq!(missing_after.radius, 1_500.0);
    let (_, gate_after) = sys_handle.get_ball_snapshot(gate_id).await.unwrap().unwrap();
    assert_eq!(gate_after.type_id, custom_gate_type);

    // P2-1 & P2-4 consistency checks: zero type_id/count entries filtered consistently, trimmed DNG-INST- resolves.
    let grp = DungeonNpcGroup {
        npc_entries: vec![
            DungeonNpcSpawnEntry { type_id: 0, count: 2, ..Default::default() },
            DungeonNpcSpawnEntry { type_id: 48_090, count: 0, ..Default::default() },
            DungeonNpcSpawnEntry { type_id: 48_090, count: 3, ..Default::default() },
        ],
        ..Default::default()
    };
    assert_eq!(grp.effective_entries().len(), 1);
    assert_eq!(grp.total_npc_count(), 3);

    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(r0_pos[0] + 5_000.0, 0.0, 0.0))
        .await;
    let trimmed = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        sys_id,
        &[PyRep::String("  DNG-INST-978003  ".into())],
    )
    .await
    .expect("trimmed DNG-INST- recognized")
    .expect("trimmed DNG-INST- activates");
    assert_eq!(trimmed.dict_get("roomID").and_then(|v| v.as_i64()), Some(9784));
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_rpc_extra_unit_tests.rs"]
mod pve_dungeon_runtime_rpc_extra_unit_tests;
