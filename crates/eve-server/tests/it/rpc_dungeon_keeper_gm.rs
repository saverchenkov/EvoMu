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

    // 3. DEGetArchetypes
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
    assert!(!rows.is_empty(), "Archetypes rowset must not be empty");

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

    // 6. DEGetRoomObjectPaletteData
    let palette_rep = rpc(&ctx, &mut sess, "dungeon", "DEGetRoomObjectPaletteData", &[])
        .await
        .expect("DEGetRoomObjectPaletteData");
    let PyRep::List(ref palette) = palette_rep else {
        panic!("Expected List from DEGetRoomObjectPaletteData");
    };
    assert!(!palette.is_empty(), "Room object palette not empty");
    let has_accel_gate = palette.iter().any(|item| match item {
        PyRep::Tuple(pair) => pair.first() == Some(&PyRep::Integer(1972)),
        _ => false,
    });
    assert!(has_accel_gate, "Palette must contain Acceleration Gate (1972)");

    // 7. DEGetRooms
    let mut room_kw = HashMap::new();
    room_kw.insert("dungeonID".to_string(), PyRep::Integer(43));
    let rooms_rep = rpc_kw(
        &ctx,
        &mut sess,
        "dungeon",
        "DEGetRooms",
        &[],
        &room_kw,
    )
    .await
    .expect("DEGetRooms");
    let PyRep::Rowset {
        columns: ref r_cols,
        rows: ref r_rows,
    } = rooms_rep
    else {
        panic!("Expected Rowset from DEGetRooms");
    };
    assert!(r_cols.contains(&"roomID".to_string()));
    assert!(r_cols.contains(&"roomName".to_string()));
    assert!(r_cols.contains(&"dungeonID".to_string()));
    assert!(!r_rows.is_empty(), "Rooms rowset not empty");
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

    // 1. keeper.GetLevelEditor (GM only)
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
    assert_eq!(
        get_dict_entry(&gm_editor_rep, "boundOID"),
        Some(&PyRep::Integer(900_001))
    );

    // 2. keeper.GetCurrentDungeonForCharacter
    let current_dungeon = rpc(&ctx, &mut sess, "keeper", "GetCurrentDungeonForCharacter", &[])
        .await
        .expect("GetCurrentDungeonForCharacter");
    // Not in dungeon pocket -> returns PyRep::None
    assert_eq!(current_dungeon, PyRep::None);

    // 3. keeper.GetOperationSpawnpointSolarSystem
    let spawn_sys = rpc(&ctx, &mut sess, "keeper", "GetOperationSpawnpointSolarSystem", &[])
        .await
        .expect("GetOperationSpawnpointSolarSystem");
    assert_eq!(spawn_sys, PyRep::Integer(SYSTEM_JITA as i64));

    // 4. keeper.IsOperationSite
    let is_op_jita = rpc(&ctx, &mut sess, "keeper", "IsOperationSite", &[PyRep::Integer(SYSTEM_JITA as i64)])
        .await
        .expect("IsOperationSite Jita");
    assert_eq!(is_op_jita, PyRep::Bool(true));

    let is_op_invalid = rpc(&ctx, &mut sess, "keeper", "IsOperationSite", &[PyRep::Integer(99_999_999)])
        .await
        .expect("IsOperationSite invalid system");
    assert_eq!(is_op_invalid, PyRep::Bool(false));

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
