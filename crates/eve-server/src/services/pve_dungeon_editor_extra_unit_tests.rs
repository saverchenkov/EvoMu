//! Additional branch-coverage and mutation-killing unit tests for `pve_dungeon_editor.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_dungeon_editor::{
    build_default_rooms_for_sde_dungeon, ensure_dungeon_rooms_seeded,
    get_dungeon_or_room_mwd_override, handle_gm_dungeon_editor_op,
    is_dungeon_or_room_mwd_disallowed, set_dungeon_disallow_mwd, set_room_disallow_mwd,
    DungeonEditorStore,
};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::Ball;
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{DungeonAsteroidEntry, DungeonTemplate, SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_editor_ctx(sys_id: SolarSystemId, char_id: i64) -> (ServerContext, SessionState) {
    let mut sde = shared_sde();
    let reg = Arc::make_mut(&mut sde);
    reg.dungeons.insert(880_001, DungeonTemplate { dungeon_id: 880_001, name: "SyntheticAsteroidDungeon".into(), faction_id: None, archetype_id: None, difficulty: None });
    reg.dungeon_asteroids.insert(
        880_001,
        vec![
            DungeonAsteroidEntry { room_id: 88_101, type_id: 1228, asteroid_count: 0 },
            DungeonAsteroidEntry { room_id: 88_101, type_id: 1230, asteroid_count: 5 },
            DungeonAsteroidEntry { room_id: 88_102, type_id: 1228, asteroid_count: 3 },
        ],
    );
    reg.dungeons.insert(880_002, DungeonTemplate { dungeon_id: 880_002, name: "SyntheticGasDungeon".into(), faction_id: None, archetype_id: None, difficulty: None });
    reg.dungeon_gas_types.insert(880_002, vec![25268, 25270]);
    reg.dungeons.insert(880_003, DungeonTemplate { dungeon_id: 880_003, name: "SyntheticEmptyDungeon".into(), faction_id: None, archetype_id: None, difficulty: None });

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dun_extra_gm", "pw", char_id, "DungeonExtraGM");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = sys_id; }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    let mut session = SessionState::new(402);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.role = service_roles::ROLE_GM_MASK;
    (ctx, session)
}

async fn call_op(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_gm_dungeon_editor_op(ctx, session, method, args, kwargs).await
}

#[test]
fn default_store_seeding_gas_asteroid_empty_and_mwd_override_precedence_kill_mutations() {
    let def_store = DungeonEditorStore::default();
    assert_eq!(def_store.next_custom_room_id, 9_000_001);
    assert_eq!(def_store.next_custom_object_id, 1_965_000_001);

    let (ctx, _) = make_editor_ctx(30_000_142, 90_050_001);

    let ast_d = ctx.sde.dungeons.get(&880_001).unwrap().clone();
    let ast_rooms = build_default_rooms_for_sde_dungeon(&ctx, &ast_d);
    assert_eq!(ast_rooms.len(), 2);
    assert_eq!(ast_rooms[0].room_id, 88_101);
    assert_eq!(ast_rooms[0].room_name, "SyntheticAsteroidDungeon - Room 1");
    assert_eq!(ast_rooms[0].objects.len(), 2);
    assert_eq!(ast_rooms[0].objects[0].object_id, 1_960_000_000 + 88_101 * 100 + 1);
    assert_eq!(ast_rooms[0].objects[0].quantity, 1);
    assert_eq!(ast_rooms[0].objects[0].radius, 2_500.0);
    assert_eq!(ast_rooms[0].objects[0].x, 0.0);
    assert_eq!(ast_rooms[0].objects[1].x, 5_000.0);
    assert_eq!(ast_rooms[0].objects[1].quantity, 5);
    assert_eq!(ast_rooms[1].room_name, "SyntheticAsteroidDungeon - Room 2");
    assert_eq!(ast_rooms[1].objects[0].z, 25_000.0);

    let gas_d = ctx.sde.dungeons.get(&880_002).unwrap().clone();
    let gas_rooms = build_default_rooms_for_sde_dungeon(&ctx, &gas_d);
    assert_eq!(gas_rooms.len(), 1);
    let expected_gas_rid = 880_002_i64 * 100 + 1;
    assert_eq!(gas_rooms[0].room_id, expected_gas_rid);
    assert_eq!(gas_rooms[0].room_name, "SyntheticGasDungeon");
    assert_eq!(gas_rooms[0].objects.len(), 2);
    assert_eq!(gas_rooms[0].objects[0].object_id, 1_960_000_000 + expected_gas_rid * 100 + 1);
    assert_eq!(gas_rooms[0].objects[0].radius, 2_000.0);
    assert_eq!(gas_rooms[0].objects[1].x, 5_000.0);

    let empty_d = ctx.sde.dungeons.get(&880_003).unwrap().clone();
    assert!(build_default_rooms_for_sde_dungeon(&ctx, &empty_d).is_empty());

    let _ = ensure_dungeon_rooms_seeded(&ctx, &ast_d);
    assert_eq!(get_dungeon_or_room_mwd_override(&ctx, 880_001, None), None);
    assert_eq!(get_dungeon_or_room_mwd_override(&ctx, 880_001, Some(999_999)), None);
    set_dungeon_disallow_mwd(&ctx, 880_001, true);
    assert_eq!(get_dungeon_or_room_mwd_override(&ctx, 880_001, Some(999_999)), Some(true));
    assert_eq!(get_dungeon_or_room_mwd_override(&ctx, 880_001, Some(88_101)), Some(false));
    assert!(is_dungeon_or_room_mwd_disallowed(&ctx, 880_001, Some(88_101)));
    set_dungeon_disallow_mwd(&ctx, 880_001, false);
    assert!(!is_dungeon_or_room_mwd_disallowed(&ctx, 880_001, Some(88_101)));
    assert!(set_room_disallow_mwd(&ctx, 88_101, true));
    assert!(is_dungeon_or_room_mwd_disallowed(&ctx, 880_001, Some(88_101)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gm_editor_error_guards_play_dungeon_goto_room_and_kwargs_kill_mutations() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_002;
    let (ctx, mut session) = make_editor_ctx(sys_id, char_id);
    let kw = HashMap::new();

    let mut unauth = SessionState::default();
    assert!(call_op(&ctx, &mut unauth, "Bind", &[], &kw).await.is_err());

    for m in ["GetCurrentRoomID", "GetEditingRoomID", "GetEditingDungeonID"] {
        assert_eq!(call_op(&ctx, &mut session, m, &[], &kw).await.unwrap(), PyRep::None);
    }

    let goto_ast = call_op(&ctx, &mut session, "GotoRoom", &[PyRep::Integer(88_102)], &kw).await.unwrap();
    assert_eq!(goto_ast.dict_get("dungeonID"), Some(&PyRep::Integer(880_001)));

    let gas_rid = 880_002_i64 * 100 + 1;
    let goto_gas = call_op(&ctx, &mut session, "GotoRoom", &[PyRep::Integer(gas_rid)], &kw).await.unwrap();
    assert_eq!(goto_gas.dict_get("dungeonID"), Some(&PyRep::Integer(880_002)));

    assert_eq!(call_op(&ctx, &mut session, "GotoRoom", &[], &kw).await.unwrap_err().message, "RoomIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "GotoRoom", &[PyRep::Integer(50)], &kw).await.unwrap_err().message, "RoomNotFound");

    session.solar_system_id = None;
    assert_eq!(call_op(&ctx, &mut session, "EditDungeon", &[PyRep::Integer(880_001)], &kw).await.unwrap_err().message, "MustBeInSpace");
    assert_eq!(call_op(&ctx, &mut session, "GotoRoom", &[PyRep::Integer(88_101)], &kw).await.unwrap_err().message, "MustBeInSpace");
    session.solar_system_id = Some(sys_id);

    assert_eq!(call_op(&ctx, &mut session, "EditDungeon", &[PyRep::Integer(0)], &kw).await.unwrap_err().message, "DungeonIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "EditDungeon", &[PyRep::Integer(999_999_999)], &kw).await.unwrap_err().message, "DungeonNotFound");
    assert_eq!(call_op(&ctx, &mut session, "EditDungeon", &[PyRep::Integer(880_003)], &kw).await.unwrap_err().message, "RoomNotFound");
    assert_eq!(call_op(&ctx, &mut session, "EditDungeon", &[PyRep::Integer(880_001), PyRep::Integer(999_999)], &kw).await.unwrap_err().message, "RoomNotFound");

    let ship_id = 555_001_i64;
    session.ship_id = Some(ship_id);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.5,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;

    let play_res = call_op(&ctx, &mut session, "PlayDungeon", &[PyRep::Integer(880_001), PyRep::Integer(88_102)], &kw).await.unwrap();
    assert_eq!(play_res.dict_get("godmode"), Some(&PyRep::Bool(false)));
    assert_eq!(play_res.dict_get("roomID"), Some(&PyRep::Integer(88_102)));
    let (_, ship_snap) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(ship_snap.position.z, 22_500.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_and_object_crud_validation_errors_and_kwargs_kill_mutations() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_003;
    let (ctx, mut session) = make_editor_ctx(sys_id, char_id);
    let kw = HashMap::new();

    assert_eq!(call_op(&ctx, &mut session, "EditRoom", &[], &kw).await.unwrap_err().message, "DungeonIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "EditRoom", &[PyRep::Integer(999_999)], &kw).await.unwrap_err().message, "DungeonNotFound");

    let mut edit_kw = HashMap::new();
    edit_kw.insert("dungeonID".into(), PyRep::Integer(880_003));
    edit_kw.insert("roomName".into(), PyRep::String("   ".into()));
    edit_kw.insert("disallowMWD".into(), PyRep::Integer(1));
    let created_room = call_op(&ctx, &mut session, "EditRoom", &[], &edit_kw).await.unwrap();
    let rid = created_room.dict_get("roomID").unwrap().as_i64().unwrap();
    assert_eq!(created_room.dict_get("roomName"), Some(&PyRep::String("New Room".into())));
    assert_eq!(created_room.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

    assert_eq!(call_op(&ctx, &mut session, "SaveRoom", &[], &kw).await.unwrap_err().message, "RoomIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "SaveRoom", &[PyRep::Integer(rid), PyRep::String("  ".into())], &kw).await.unwrap_err().message, "RoomNameRequired");
    assert_eq!(call_op(&ctx, &mut session, "SaveRoom", &[PyRep::Integer(999_999_999), PyRep::String("Valid".into())], &kw).await.unwrap_err().message, "RoomNotFound");

    let mut save_kw = HashMap::new();
    save_kw.insert("disallowMWD".into(), PyRep::Bool(false));
    let saved_room = call_op(&ctx, &mut session, "SaveRoom", &[PyRep::Integer(rid), PyRep::String("UpdatedName".into())], &save_kw).await.unwrap();
    assert_eq!(saved_room.dict_get("disallowMWD"), Some(&PyRep::Bool(false)));

    assert_eq!(call_op(&ctx, &mut session, "GetDunObjects", &[], &kw).await.unwrap_err().message, "RoomIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "DropIntoDungeonRoom", &[], &kw).await.unwrap_err().message, "RoomIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "DropIntoDungeonRoom", &[PyRep::Integer(rid), PyRep::Integer(999_999_999)], &kw).await.unwrap_err().message, "InvalidTypeID");

    let _ = call_op(&ctx, &mut session, "GotoRoom", &[PyRep::Integer(rid)], &kw).await.unwrap();
    let dropped = call_op(
        &ctx,
        &mut session,
        "DropIntoDungeonRoom",
        &[
            PyRep::None, PyRep::Integer(1228), PyRep::Real(10.0), PyRep::Real(20.0), PyRep::Real(30.0),
            PyRep::Real(1.0), PyRep::Real(2.0), PyRep::Real(3.0), PyRep::Real(-5.0), PyRep::Integer(0),
        ],
        &kw,
    ).await.unwrap();
    let oid = dropped.dict_get("objectID").unwrap().as_i64().unwrap();
    assert_eq!(dropped.dict_get("radius"), Some(&PyRep::Real(1_500.0)));
    assert_eq!(dropped.dict_get("quantity"), Some(&PyRep::Integer(1)));

    for m in [
        "SetObjectPosition", "GetObjectPosition", "SetObjectRotation", "GetObjectRotation",
        "SetObjectRadius", "GetObjectRadius", "SetObjectQuantity", "GetObjectQuantity",
    ] {
        assert_eq!(call_op(&ctx, &mut session, m, &[], &kw).await.unwrap_err().message, "ObjectIDRequired");
    }
    assert_eq!(call_op(&ctx, &mut session, "SetObjectPosition", &[PyRep::Integer(oid), PyRep::Real(1.0)], &kw).await.unwrap_err().message, "CoordinatesRequired");
    assert_eq!(call_op(&ctx, &mut session, "SetObjectRadius", &[PyRep::Integer(oid), PyRep::Real(0.0)], &kw).await.unwrap_err().message, "InvalidRadius");
    for m in ["GetObjectPosition", "GetObjectRotation", "GetObjectRadius", "GetObjectQuantity"] {
        assert_eq!(call_op(&ctx, &mut session, m, &[PyRep::Integer(999_999_999)], &kw).await.unwrap_err().message, "ObjectNotFound");
    }

    let sel = call_op(&ctx, &mut session, "ObjectSelection", &[PyRep::Tuple(vec![PyRep::Integer(oid)])], &kw).await.unwrap();
    assert_eq!(sel, PyRep::List(vec![PyRep::Integer(oid)]));

    assert_eq!(call_op(&ctx, &mut session, "DeleteGroupObjects", &[PyRep::Integer(999_999_999)], &kw).await.unwrap_err().message, "ObjectNotFound");
    assert_eq!(call_op(&ctx, &mut session, "DeleteGroupObjects", &[PyRep::List(vec![PyRep::Integer(oid)])], &kw).await.unwrap(), PyRep::Bool(true));
    assert_eq!(call_op(&ctx, &mut session, "DeleteSelected", &[], &kw).await.unwrap_err().message, "ObjectNotFound");

    assert_eq!(call_op(&ctx, &mut session, "DeleteGroup", &[], &kw).await.unwrap_err().message, "RoomIDRequired");
    assert_eq!(call_op(&ctx, &mut session, "DeleteGroup", &[PyRep::Integer(999_999_999)], &kw).await.unwrap_err().message, "RoomNotFound");
    assert_eq!(call_op(&ctx, &mut session, "ResetDungeon", &[PyRep::Integer(880_003)], &kw).await.unwrap(), PyRep::Bool(true));
    assert_eq!(call_op(&ctx, &mut session, "UnknownOp", &[], &kw).await.unwrap_err().message, "UnimplementedRPC");
}
