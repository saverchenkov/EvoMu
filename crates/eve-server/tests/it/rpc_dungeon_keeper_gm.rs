//! Integration tests for Dungeon GM Editor (`dungeon`), Keeper services (`keeper`), and Alpha Injector (`alphaInjectorMgr`).

use crate::common;
use eve_db::service_roles::ROLE_GM_DEFAULT;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const SYSTEM_JITA: u32 = 30_000_142;

fn get_dict_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if let PyRep::String(s) | PyRep::Token(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None).await
}

async fn setup_pilot_session(ctx: &ServerContext) -> SessionState {
    let mut sess = SessionState::new(4301);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(SYSTEM_JITA);
    sess
}

#[tokio::test]
async fn test_gm_dungeon_editor_methods() {
    // parity: rpc:dungeon.DEGetArchetypes
    // parity: rpc:dungeon.DEGetDungeons
    // parity: rpc:dungeon.DEGetFactions
    // parity: rpc:dungeon.DEGetRoomObjectPaletteData
    // parity: rpc:dungeon.DEGetRooms
    // parity: feat:admin.gm-dungeon-editor
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // 1. As regular pilot (non-GM): all GM editor calls MUST fail with InsufficientRole
    for (svc, method) in [
        ("dungeon", "DEGetArchetypes"),
        ("dungeon", "DEGetDungeons"),
        ("dungeon", "DEGetFactions"),
        ("dungeon", "DEGetRoomObjectPaletteData"),
        ("dungeon", "DEGetRooms"),
    ] {
        let err = rpc(&ctx, &mut sess, svc, method, &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.code, "InsufficientRole",
            "{svc}.{method} should require GM role"
        );
    }

    // 2. Elevate session to Content GM (`feat:admin.gm-dungeon-editor`)
    sess.role = ROLE_GM_DEFAULT;

    // 3. DEGetArchetypes: returns the exact set of distinct SDE dungeon archetype_ids
    let archetypes_rep = rpc(&ctx, &mut sess, "dungeon", "DEGetArchetypes", &[])
        .await
        .expect("DEGetArchetypes as GM");
    let PyRep::Rowset {
        ref columns,
        ref rows,
    } = archetypes_rep
    else {
        panic!("Expected Rowset from DEGetArchetypes");
    };
    assert!(columns.contains(&"archetypeID".to_string()));
    assert!(columns.contains(&"archetypeName".to_string()));
    let returned_aids: std::collections::BTreeSet<i64> = rows
        .iter()
        .filter_map(|r| r.first().and_then(|v| v.as_i64()))
        .collect();
    let expected_aids: std::collections::BTreeSet<i64> = ctx
        .sde
        .dungeons
        .values()
        .filter_map(|d| d.archetype_id.map(i64::from))
        .collect();
    assert_eq!(
        returned_aids, expected_aids,
        "DEGetArchetypes must return the exact set of archetype_ids present on SDE dungeons"
    );

    // 4. DEGetDungeons (unfiltered & filtered)
    let dungeons_all = rpc(&ctx, &mut sess, "dungeon", "DEGetDungeons", &[])
        .await
        .expect("DEGetDungeons all");
    let PyRep::Rowset {
        columns: ref d_cols,
        rows: ref d_rows,
    } = dungeons_all
    else {
        panic!("Expected Rowset from DEGetDungeons");
    };
    assert!(d_cols.contains(&"dungeonID".to_string()));
    assert!(d_cols.contains(&"dungeonName".to_string()));
    assert!(!d_rows.is_empty(), "Dungeons rowset must not be empty");

    // Filter by specific dungeonID = 43 ("Pith Merchant Depot")
    let mut kw = HashMap::new();
    kw.insert("dungeonID".to_string(), PyRep::Integer(43));
    let dungeon_43_rep = rpc_kw(
        &ctx,
        &mut sess,
        "dungeon",
        "DEGetDungeons",
        &[],
        &kw,
    )
    .await
    .expect("DEGetDungeons for dungeon 43");
    let PyRep::Rowset { rows: ref d43_rows, .. } = dungeon_43_rep else {
        panic!("Expected Rowset");
    };
    assert_eq!(d43_rows.len(), 1, "Expected exactly 1 dungeon for ID 43");
    assert_eq!(d43_rows[0][0], PyRep::Integer(43));

    // 5. DEGetFactions
    let factions_rep = rpc(&ctx, &mut sess, "dungeon", "DEGetFactions", &[])
        .await
        .expect("DEGetFactions");
    let PyRep::Rowset {
        columns: ref f_cols,
        rows: ref f_rows,
    } = factions_rep
    else {
        panic!("Expected Rowset from DEGetFactions");
    };
    assert!(f_cols.contains(&"factionID".to_string()));
    assert!(f_cols.contains(&"factionName".to_string()));
    assert!(
        f_rows.len() >= 20,
        "Should return all SDE factions (got {})",
        f_rows.len()
    );

    // 6. DEGetRoomObjectPaletteData: dynamically populated from SDE types (Stargates, Beacons, Warp/Acceleration Gates)
    let palette_rep = rpc(&ctx, &mut sess, "dungeon", "DEGetRoomObjectPaletteData", &[])
        .await
        .expect("DEGetRoomObjectPaletteData");
    let PyRep::List(ref palette) = palette_rep else {
        panic!("Expected List from DEGetRoomObjectPaletteData");
    };
    assert!(palette.len() >= 50, "Room object palette should contain SDE celestial/gate types");
    let has_accel_gate = palette.iter().any(|item| match item {
        PyRep::Tuple(pair) => {
            pair.first() == Some(&PyRep::Integer(17831))
                && pair.get(1) == Some(&PyRep::String("Acceleration Gate".into()))
        }
        _ => false,
    });
    assert!(has_accel_gate, "Palette must contain SDE Acceleration Gate (17831)");
    for item in palette {
        let PyRep::Tuple(pair) = item else { panic!("expected Tuple in palette") };
        let tid = pair[0].as_i64().unwrap() as u32;
        let tname = pair[1].as_str().unwrap();
        let sde_type = ctx.sde.types.get(&tid).expect("palette type must exist in SDE");
        assert_eq!(sde_type.name, tname);
    }

    // 7. DEGetRooms: requires valid SDE dungeonID and returns real SDE room IDs from sde_dungeon_asteroid_counts
    assert_eq!(
        rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[]).await.unwrap_err().code,
        "DungeonIDRequired"
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[PyRep::Integer(999_999_999)]).await.unwrap_err().code,
        "DungeonNotFound"
    );
    let mut room_kw = HashMap::new();
    room_kw.insert("dungeonID".to_string(), PyRep::Integer(49));
    let rooms_rep = rpc_kw(
        &ctx,
        &mut sess,
        "dungeon",
        "DEGetRooms",
        &[],
        &room_kw,
    )
    .await
    .expect("DEGetRooms for dungeon 49");
    let PyRep::Rowset {
        columns: ref r_cols,
        rows: ref r_rows,
    } = rooms_rep
    else {
        panic!("Expected Rowset from DEGetRooms");
    };
    assert_eq!(r_cols, &vec!["roomID".to_string(), "roomName".to_string(), "dungeonID".to_string()]);
    let room_ids: Vec<i64> = r_rows.iter().map(|r| r[0].as_i64().unwrap()).collect();
    assert_eq!(room_ids, vec![8652, 8653, 8654]);

    // 8. DEGetRooms on non-asteroid SDE dungeon (43 = "Pith Merchant Depot") + full GM room & object CRUD
    let rooms_43 = rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[PyRep::Integer(43)])
        .await
        .expect("DEGetRooms for non-asteroid dungeon 43");
    let PyRep::Rowset { rows: ref r43_rows, .. } = rooms_43 else { panic!("Expected Rowset") };
    assert!(r43_rows.is_empty(), "Non-asteroid/non-gas SDE dungeon 43 starts with 0 rooms");

    // Create a custom room in dungeon 43, rename it, and verify DEGetRooms reflects the persisted room
    let created_room = rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "EditRoom",
        &[PyRep::Integer(43), PyRep::String("Inner Sanctum".into())],
    )
    .await
    .expect("EditRoom");
    let new_room_id = get_dict_entry(&created_room, "roomID").and_then(|v| v.as_i64()).expect("roomID");
    rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "SaveRoom",
        &[PyRep::Integer(new_room_id), PyRep::String("Pith Command Core".into())],
    )
    .await
    .expect("SaveRoom");
    let rooms_43_after = rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[PyRep::Integer(43)])
        .await
        .expect("DEGetRooms after create+rename");
    let PyRep::Rowset { rows: ref r43_after_rows, .. } = rooms_43_after else { panic!("Expected Rowset") };
    assert_eq!(r43_after_rows.len(), 1);
    assert_eq!(r43_after_rows[0][1], PyRep::String("Pith Command Core".into()));

    // Enter level editor on room new_room_id, place an Acceleration Gate (17831), move/rotate/resize it, and verify ballpark
    rpc(
        &ctx,
        &mut sess,
        "keeper",
        "EditDungeon",
        &[PyRep::Integer(43), PyRep::Integer(new_room_id)],
    )
    .await
    .expect("keeper.EditDungeon");
    assert_eq!(
        rpc(&ctx, &mut sess, "keeper", "GetCurrentlyEditedRoomID", &[]).await.expect("GetCurrentlyEditedRoomID"),
        PyRep::Integer(new_room_id)
    );
    let created_obj = rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "DropIntoDungeonRoom",
        &[
            PyRep::Integer(new_room_id),
            PyRep::Integer(17831),
            PyRep::Real(12_000.0),
            PyRep::Real(-3_000.0),
            PyRep::Real(8_000.0),
        ],
    )
    .await
    .expect("DropIntoDungeonRoom");
    let obj_id = get_dict_entry(&created_obj, "objectID").and_then(|v| v.as_i64()).expect("objectID");
    let created_obj2 = rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "DropIntoDungeonRoom",
        &[
            PyRep::Integer(new_room_id),
            PyRep::Integer(10246),
            PyRep::Real(5_000.0),
            PyRep::Real(0.0),
            PyRep::Real(2_000.0),
        ],
    )
    .await
    .expect("DropIntoDungeonRoom 2");
    let obj2_id = get_dict_entry(&created_obj2, "objectID").and_then(|v| v.as_i64()).expect("objectID 2");
    let jita_handle = ctx.systems.get_or_spawn(SYSTEM_JITA);
    let (_, ball_before) = jita_handle.get_ball_snapshot(obj_id).await.unwrap().expect("editor ball spawned in Jita");
    assert_eq!(ball_before.position, glam::DVec3::new(12_000.0, -3_000.0, 8_000.0));

    rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "SetObjectPosition",
        &[
            PyRep::Integer(obj_id),
            PyRep::Real(25_000.0),
            PyRep::Real(4_000.0),
            PyRep::Real(-6_000.0),
        ],
    )
    .await
    .expect("SetObjectPosition");
    rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "SetObjectRotation",
        &[PyRep::Integer(obj_id), PyRep::Real(45.0), PyRep::Real(15.0), PyRep::Real(0.0)],
    )
    .await
    .expect("SetObjectRotation");
    rpc(
        &ctx,
        &mut sess,
        "dungeon",
        "SetObjectRadius",
        &[PyRep::Integer(obj_id), PyRep::Real(3_200.0)],
    )
    .await
    .expect("SetObjectRadius");
    let (_, ball_after) = jita_handle.get_ball_snapshot(obj_id).await.unwrap().expect("editor ball updated");
    assert_eq!(ball_after.position, glam::DVec3::new(25_000.0, 4_000.0, -6_000.0));
    assert_eq!(ball_after.radius, 3_200.0);

    let room_objs = rpc(&ctx, &mut sess, "keeper", "GetDunObjects", &[PyRep::Integer(new_room_id)])
        .await
        .expect("keeper.GetDunObjects");
    let PyRep::List(ref objs_list) = room_objs else { panic!("Expected List from GetDunObjects") };
    assert_eq!(objs_list.len(), 2);
    assert_eq!(get_dict_entry(&objs_list[0], "yaw"), Some(&PyRep::Real(45.0)));

    // Select both objects via ObjectSelection and delete them with no-arg DeleteSelected()
    rpc(
        &ctx,
        &mut sess,
        "keeper",
        "ObjectSelection",
        &[PyRep::List(vec![PyRep::Integer(obj_id), PyRep::Integer(obj2_id)])],
    )
    .await
    .expect("keeper.ObjectSelection");
    rpc(&ctx, &mut sess, "dungeon", "DeleteSelected", &[]).await.expect("no-arg DeleteSelected");
    assert!(jita_handle.get_ball_snapshot(obj_id).await.unwrap().is_none(), "deleted editor object 1 removed from ballpark");
    assert!(jita_handle.get_ball_snapshot(obj2_id).await.unwrap().is_none(), "deleted editor object 2 removed from ballpark");

    // ResetDungeon restores dungeon 43's room list back to SDE defaults (0 rooms)
    rpc(&ctx, &mut sess, "keeper", "ResetDungeon", &[PyRep::Integer(43)]).await.expect("keeper.ResetDungeon");
    let rooms_43_reset = rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[PyRep::Integer(43)]).await.expect("DEGetRooms after ResetDungeon");
    let PyRep::Rowset { rows: ref r43_reset_rows, .. } = rooms_43_reset else { panic!("Expected Rowset") };
    assert!(r43_reset_rows.is_empty(), "ResetDungeon restores dungeon 43 rooms back to SDE default");
    assert_eq!(
        rpc(&ctx, &mut sess, "keeper", "GetCurrentlyEditedRoomID", &[]).await.expect("GetCurrentlyEditedRoomID after Reset"),
        PyRep::None
    );
}

#[tokio::test]
async fn test_keeper_and_alpha_injector_services() {
    // parity: rpc:keeper.GetCurrentDungeonForCharacter
    // parity: rpc:keeper.GetLevelEditor
    // parity: rpc:keeper.GetOperationSpawnpointSolarSystem
    // parity: rpc:keeper.IsOperationSite
    // parity: rpc:alphaInjectorMgr.GetNextAvailableInjection
    // parity: feat:skills.alpha-injector
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // 1. keeper.GetLevelEditor (GM only, binds a real MachoNet bound object in ctx.bound_objects)
    let non_gm_err = rpc(&ctx, &mut sess, "keeper", "GetLevelEditor", &[])
        .await
        .unwrap_err();
    assert_eq!(non_gm_err.code, "InsufficientRole");

    sess.role = ROLE_GM_DEFAULT;
    let gm_editor_rep = rpc(&ctx, &mut sess, "keeper", "GetLevelEditor", &[])
        .await
        .expect("GetLevelEditor as GM");
    assert_eq!(
        get_dict_entry(&gm_editor_rep, "isEditorActive"),
        Some(&PyRep::Bool(true))
    );
    let bound_oid = get_dict_entry(&gm_editor_rep, "boundOID")
        .and_then(|v| v.as_i64())
        .expect("boundOID must be an integer");
    assert!(bound_oid > 0);
    assert!(
        ctx.verify_bound_object(&sess, bound_oid as u64).is_ok(),
        "keeper.GetLevelEditor boundOID must be registered in ctx.bound_objects"
    );

    // 2. keeper.GetCurrentDungeonForCharacter: None in normal space; 4-tuple when in an active dungeon site
    let current_dungeon = rpc(&ctx, &mut sess, "keeper", "GetCurrentDungeonForCharacter", &[])
        .await
        .expect("GetCurrentDungeonForCharacter");
    assert_eq!(current_dungeon, PyRep::None);

    assert!(
        rpc(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(999_999_999)])
            .await
            .is_err(),
        "start_site must reject non-existent dungeon IDs"
    );
    rpc(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(49)])
        .await
        .expect("start_site 49");
    let active_dng = rpc(&ctx, &mut sess, "keeper", "GetCurrentDungeonForCharacter", &[])
        .await
        .expect("GetCurrentDungeonForCharacter in site 49");
    let PyRep::Tuple(dng_parts) = active_dng else {
        panic!("expected 4-tuple from GetCurrentDungeonForCharacter, got {active_dng:?}");
    };
    assert_eq!(dng_parts.len(), 4);
    assert_eq!(dng_parts[0], PyRep::Integer(49));
    assert_eq!(dng_parts[1], PyRep::Integer(8652));
    assert_eq!(dng_parts[2], PyRep::Integer(49));

    // 3. keeper.GetOperationSpawnpointSolarSystem: resolves SDE starter school system, or activated operation system
    let mut sess2 = setup_pilot_session(&ctx).await;
    assert!(
        rpc(
            &ctx,
            &mut sess2,
            "keeper",
            "GetOperationSpawnpointSolarSystem",
            &[PyRep::Integer(90_000_002), PyRep::Integer(999)],
        )
        .await
        .is_err(),
        "non-GM character must not query another character's operation spawnpoint solar system"
    );
    let spawn_sys = rpc(
        &ctx,
        &mut sess2,
        "keeper",
        "GetOperationSpawnpointSolarSystem",
        &[PyRep::Integer(CHAR_ID), PyRep::Integer(999)],
    )
    .await
    .expect("GetOperationSpawnpointSolarSystem");
    assert_eq!(spawn_sys, PyRep::Integer(30_002_547)); // Malukker (Republic Military School 14 in SDE)

    assert!(
        rpc(
            &ctx,
            &mut sess2,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[PyRep::Integer(501), PyRep::Integer(30_002_187)],
        )
        .await
        .is_err(),
        "activate_operation_in_solar_system must reject a solar system where the character is not present"
    );
    sess2.solar_system_id = Some(30_002_187);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.solar_system_id = 30_002_187;
        }
    });
    rpc(
        &ctx,
        &mut sess2,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(501), PyRep::Integer(30_002_187)],
    )
    .await
    .expect("activate_operation_in_solar_system");
    let spawn_sys_active = rpc(
        &ctx,
        &mut sess2,
        "keeper",
        "GetOperationSpawnpointSolarSystem",
        &[PyRep::Integer(CHAR_ID), PyRep::Integer(501)],
    )
    .await
    .expect("GetOperationSpawnpointSolarSystem active");
    assert_eq!(spawn_sys_active, PyRep::Integer(30_002_187));

    // 4. keeper.IsOperationSite: scoped to calling character and their current solar system
    let is_op_jita = rpc(&ctx, &mut sess2, "keeper", "IsOperationSite", &[PyRep::Integer(SYSTEM_JITA as i64)])
        .await
        .expect("IsOperationSite Jita");
    assert_eq!(is_op_jita, PyRep::Bool(false));

    let is_op_site_id = rpc(&ctx, &mut sess2, "keeper", "IsOperationSite", &[PyRep::Integer(501)])
        .await
        .expect("IsOperationSite active operation 501");
    assert_eq!(is_op_site_id, PyRep::Bool(true));

    let is_op_activated = rpc(&ctx, &mut sess2, "keeper", "IsOperationSite", &[PyRep::Integer(30_002_187)])
        .await
        .expect("IsOperationSite activated system");
    assert_eq!(is_op_activated, PyRep::Bool(true));

    let is_op_invalid = rpc(&ctx, &mut sess2, "keeper", "IsOperationSite", &[PyRep::Integer(99_999_999)])
        .await
        .expect("IsOperationSite invalid system");
    assert_eq!(is_op_invalid, PyRep::Bool(false));

    // Jumping back to Jita invalidates the stale operation site/system for IsOperationSite and GetCurrentDungeonForCharacter
    sess2.solar_system_id = Some(SYSTEM_JITA);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.solar_system_id = SYSTEM_JITA;
        }
    });
    let is_op_after_jump = rpc(&ctx, &mut sess2, "keeper", "IsOperationSite", &[PyRep::Integer(30_002_187)])
        .await
        .expect("IsOperationSite after leaving system");
    assert_eq!(is_op_after_jump, PyRep::Bool(false));

    // 5. alphaInjectorMgr.GetNextAvailableInjection initially available (0 ticks)
    let next_avail_rep = rpc(&ctx, &mut sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection initially");
    assert_eq!(next_avail_rep, PyRep::Integer(0));

    // 6. Seed character with Daily Alpha Injector (type 47414) in station hangar
    let injector_item_id = 95_000_101;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.clone_grade = 1; // Alpha clone
            ch.skill_points = 4_500_000.0;
            ch.free_skill_points = 0.0;
        }
        snap.items.insert(
            injector_item_id,
            eve_db::InventoryItemRecord {
                item_id: injector_item_id,
                type_id: 47414, // Daily Alpha Injector
                owner_id: CHAR_ID,
                location_id: 60_003_760, // Station
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Inject Daily Alpha Injector
    let inject_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillpoints",
        &[PyRep::Integer(injector_item_id)],
    )
    .await
    .expect("Inject Daily Alpha Injector");
    assert_eq!(inject_rep, PyRep::Real(50_000.0));

    // Verify free SP increased by 50,000
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.free_skill_points, 50_000.0);
    });

    // 7. Verify GetNextAvailableInjection now returns active 24h cooldown
    let next_avail_after = rpc(&ctx, &mut sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection after use");
    let PyRep::Integer(cooldown_ticks) = next_avail_after else {
        panic!("Expected Integer cooldown ticks");
    };
    let now_ft = ctx.now_blueos_filetime_ticks();
    assert!(
        cooldown_ticks > now_ft,
        "Cooldown timestamp {cooldown_ticks} must be > now_ft {now_ft}"
    );

    // 8. Attempting to consume a second Daily Alpha Injector immediately fails with cooldown active
    let injector2_item_id = 95_000_102;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            injector2_item_id,
            eve_db::InventoryItemRecord {
                item_id: injector2_item_id,
                type_id: 47414,
                owner_id: CHAR_ID,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err_cd = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillpoints",
        &[PyRep::Integer(injector2_item_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cd.code, "DailyAlphaInjectorCooldownActive");

    // 9. Omega clones cannot use Daily Alpha Injector (`feat:skills.alpha-injector`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.clone_grade = 2; // Omega clone
        }
    });
    let err_omega = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillpoints",
        &[PyRep::Integer(injector2_item_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_omega.code, "AlphaCloneRequired");
}
