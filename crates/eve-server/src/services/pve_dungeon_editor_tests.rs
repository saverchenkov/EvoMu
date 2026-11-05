//! Unit and integration tests for `pve_dungeon_editor`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_gm_editor_ctx(sys_id: SolarSystemId, char_id: i64, is_gm: bool) -> (ServerContext, SessionState) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dun_gm", "pw", char_id, "DungeonGM");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_300_000)));

    let mut session = SessionState::new(401);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    if is_gm {
        session.role = service_roles::ROLE_GM_MASK;
    }
    (ctx, session)
}

#[test]
fn dungeon_mwd_override_hierarchy_and_default_room_seeding() {
    let sys_id: SolarSystemId = 30_000_142;
    let (ctx, _) = setup_gm_editor_ctx(sys_id, 90_040_001, true);
    let template = ctx
        .sde
        .dungeon_asteroids
        .keys()
        .chain(ctx.sde.dungeon_gas_types.keys())
        .filter_map(|did| ctx.sde.dungeons.get(did))
        .next()
        .cloned()
        .expect("SDE has at least one dungeon with seeded rooms");
    let did = template.dungeon_id as i64;

    // Seed default rooms for dungeon
    let rooms = ensure_dungeon_rooms_seeded(&ctx, &template);
    assert!(!rooms.is_empty());
    let first_rid = rooms[0].room_id;
    let baseline = is_dungeon_or_room_mwd_disallowed(&ctx, did, Some(first_rid));
    assert_eq!(
        get_dungeon_or_room_mwd_override(&ctx, did, Some(first_rid)),
        Some(baseline)
    );

    // Room-level override takes precedence over dungeon-level override
    set_dungeon_disallow_mwd(&ctx, did, false);
    assert!(set_room_disallow_mwd(&ctx, first_rid, true));
    assert!(is_dungeon_or_room_mwd_disallowed(&ctx, did, Some(first_rid)));
    assert!(!set_room_disallow_mwd(&ctx, -999_999, true));

    // When no room matches, dungeon override applies
    assert!(!is_dungeon_or_room_mwd_disallowed(&ctx, did, None));
    set_dungeon_disallow_mwd(&ctx, did, true);
    assert!(is_dungeon_or_room_mwd_disallowed(&ctx, did, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gm_dungeon_editor_lifecycle_and_object_crud() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_002;
    let empty_kw = HashMap::new();

    // Non-GM session is rejected
    let (ctx_non_gm, mut non_gm_session) = setup_gm_editor_ctx(sys_id, char_id, false);
    let denied = handle_gm_dungeon_editor_op(&ctx_non_gm, &mut non_gm_session, "Bind", &[], &empty_kw).await;
    assert_eq!(denied.unwrap_err().code, "InsufficientRole");

    // GM session full workflow
    let (ctx, mut session) = setup_gm_editor_ctx(sys_id, char_id, true);
    let any_did = *ctx
        .sde
        .dungeon_asteroids
        .keys()
        .chain(ctx.sde.dungeon_gas_types.keys())
        .next()
        .expect("SDE dungeon with rooms") as i64;
    let any_tid = *ctx.sde.types.keys().next().expect("SDE type") as i64;

    let bound = handle_gm_dungeon_editor_op(&ctx, &mut session, "Bind", &[], &empty_kw)
        .await
        .expect("Bind succeeds");
    assert!(!matches!(bound, PyRep::None));

    // EditDungeon & PlayDungeon
    let edited = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "EditDungeon",
        &[PyRep::Integer(any_did)],
        &empty_kw,
    )
    .await
    .expect("EditDungeon succeeds");
    assert!(!matches!(edited, PyRep::None));

    let cur_did = handle_gm_dungeon_editor_op(&ctx, &mut session, "GetEditingDungeonID", &[], &empty_kw)
        .await
        .expect("GetEditingDungeonID");
    assert_eq!(cur_did, PyRep::Integer(any_did));

    // EditRoom (create custom room) & GotoRoom & SaveRoom
    let new_room = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "EditRoom",
        &[PyRep::Integer(any_did), PyRep::String("Vault Room".into())],
        &empty_kw,
    )
    .await
    .expect("EditRoom");
    assert!(!matches!(new_room, PyRep::None));

    let custom_rid = with_dungeon_editor_store(&ctx, |store| {
        store
            .rooms_by_dungeon
            .get(&any_did)
            .and_then(|rs| rs.last().map(|r| r.room_id))
            .expect("created room")
    });

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "GotoRoom",
        &[PyRep::Integer(custom_rid)],
        &empty_kw,
    )
    .await
    .expect("GotoRoom");
    assert_eq!(
        handle_gm_dungeon_editor_op(&ctx, &mut session, "GetCurrentlyEditedRoomID", &[], &empty_kw)
            .await
            .expect("GetCurrentlyEditedRoomID"),
        PyRep::Integer(custom_rid)
    );

    let saved = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "SaveRoom",
        &[PyRep::Integer(custom_rid), PyRep::String("Vault Renamed".into())],
        &empty_kw,
    )
    .await
    .expect("SaveRoom");
    assert!(!matches!(saved, PyRep::None));

    // DropIntoDungeonRoom & object property get/set
    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "DropIntoDungeonRoom",
        &[
            PyRep::Integer(custom_rid),
            PyRep::Integer(any_tid),
            PyRep::Real(100.0),
            PyRep::Real(200.0),
            PyRep::Real(300.0),
        ],
        &empty_kw,
    )
    .await
    .expect("DropIntoDungeonRoom");

    let oid = with_dungeon_editor_store(&ctx, |store| {
        store
            .rooms_by_dungeon
            .get(&any_did)
            .and_then(|rs| rs.iter().find(|r| r.room_id == custom_rid))
            .and_then(|r| r.objects.first().map(|o| o.object_id))
            .expect("dropped object")
    });

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "SetObjectPosition",
        &[
            PyRep::Integer(oid),
            PyRep::Real(10.0),
            PyRep::Real(20.0),
            PyRep::Real(30.0),
        ],
        &empty_kw,
    )
    .await
    .expect("SetObjectPosition");
    let pos = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "GetObjectPosition",
        &[PyRep::Integer(oid)],
        &empty_kw,
    )
    .await
    .expect("GetObjectPosition");
    assert_eq!(
        pos,
        PyRep::Tuple(vec![PyRep::Real(10.0), PyRep::Real(20.0), PyRep::Real(30.0)])
    );

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "SetObjectRotation",
        &[PyRep::Integer(oid), PyRep::Real(1.0), PyRep::Real(0.5), PyRep::Real(0.25)],
        &empty_kw,
    )
    .await
    .expect("SetObjectRotation");
    assert!(matches!(
        handle_gm_dungeon_editor_op(&ctx, &mut session, "GetObjectRotation", &[PyRep::Integer(oid)], &empty_kw)
            .await
            .expect("GetObjectRotation"),
        PyRep::Tuple(_)
    ));

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "SetObjectRadius",
        &[PyRep::Integer(oid), PyRep::Real(750.0)],
        &empty_kw,
    )
    .await
    .expect("SetObjectRadius");
    assert_eq!(
        handle_gm_dungeon_editor_op(&ctx, &mut session, "GetObjectRadius", &[PyRep::Integer(oid)], &empty_kw)
            .await
            .expect("GetObjectRadius"),
        PyRep::Real(750.0)
    );

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "SetObjectQuantity",
        &[PyRep::Integer(oid), PyRep::Integer(4)],
        &empty_kw,
    )
    .await
    .expect("SetObjectQuantity");
    assert_eq!(
        handle_gm_dungeon_editor_op(&ctx, &mut session, "GetObjectQuantity", &[PyRep::Integer(oid)], &empty_kw)
            .await
            .expect("GetObjectQuantity"),
        PyRep::Integer(4)
    );

    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "ObjectSelection",
        &[PyRep::Integer(oid)],
        &empty_kw,
    )
    .await
    .expect("ObjectSelection");
    let _ = handle_gm_dungeon_editor_op(&ctx, &mut session, "SaveAllChanges", &[], &empty_kw)
        .await
        .expect("SaveAllChanges");
    let _ = handle_gm_dungeon_editor_op(&ctx, &mut session, "IsObjectLocked", &[PyRep::Integer(oid)], &empty_kw)
        .await
        .expect("IsObjectLocked");
    let objs = handle_gm_dungeon_editor_op(&ctx, &mut session, "GetDunObjects", &[], &empty_kw)
        .await
        .expect("GetDunObjects");
    assert!(matches!(objs, PyRep::List(ref items) if !items.is_empty()));

    let _ = handle_gm_dungeon_editor_op(&ctx, &mut session, "DeleteSelected", &[], &empty_kw)
        .await
        .expect("DeleteSelected");
    let _ = handle_gm_dungeon_editor_op(
        &ctx,
        &mut session,
        "DeleteGroup",
        &[PyRep::Integer(custom_rid)],
        &empty_kw,
    )
    .await
    .expect("DeleteGroup");
    let _ = handle_gm_dungeon_editor_op(&ctx, &mut session, "Reset", &[], &empty_kw)
        .await
        .expect("Reset");
}
