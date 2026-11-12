//! Extra unit and mutation-killing tests for `services::config_service`.

#![forbid(unsafe_code)]

use crate::router::{NotificationRecord, ServerContext};
use crate::services::config_service::*;
use eve_db::{
    service_roles::ROLE_GML, AllianceRecord, CorporationRecord, InventoryItemRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, NpcCorporationInfo, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn auth_session(char_id: i64, corp_id: i64, sys_id: Option<u32>) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s
}

#[test]
fn object_caching_get_and_invalidate_role_and_argument_variants() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut unauth = SessionState::default();

    for m in [
        "GetCachableObject",
        "GetCachedObject",
        "InvalidateCachedObjects",
    ] {
        assert!(handle(&ctx, &mut unauth, "objectCaching", m, &[], &kw)
            .unwrap()
            .is_err());
    }

    let mut sess = auth_session(90_000_001, 98_000_001, Some(30_000_142));

    // Empty args -> InvalidArgument
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "objectCaching",
            "GetCachableObject",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Non-existent cached object -> CachedObjectNotFound (tested across Bool, Integer, and single-arg forms)
    for args in [
        vec![PyRep::Bool(true), PyRep::String("missing.obj".into())],
        vec![PyRep::Integer(1), PyRep::String("missing.obj".into())],
        vec![PyRep::String("missing.obj".into())],
    ] {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "objectCaching",
                "GetCachedObject",
                &args,
                &kw
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CachedObjectNotFound"
        );
    }

    // Register a cached object and retrieve + invalidate it
    let obj_id = PyRep::String("config.StaticOwners".into());
    ctx.put_cached_object(obj_id.clone(), PyRep::Integer(42), true);
    let cached = handle(
        &ctx,
        &mut sess,
        "objectCaching",
        "GetCachableObject",
        &[PyRep::Bool(true), obj_id.clone()],
        &kw,
    )
    .unwrap()
    .expect("cached object hit");
    assert!(matches!(cached, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Tuple(_)));

    // InvalidateCachedObjects without GM role -> InsufficientRole
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "objectCaching",
            "InvalidateCachedObjects",
            &[obj_id.clone()],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    // Grant GM role: test single object, list of objects, and empty args
    sess.role = ROLE_GML;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "objectCaching",
            "InvalidateCachedObjects",
            &[obj_id.clone()],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    ctx.put_cached_object(obj_id.clone(), PyRep::Integer(99), true);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "objectCaching",
            "InvalidateCachedObjects",
            &[PyRep::List(vec![obj_id, PyRep::String("none".into())])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "objectCaching",
            "InvalidateCachedObjects",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
}

#[test]
fn config_owners_locations_tickers_and_station_solar_systems_coverage() {
    let mut ctx = test_ctx();
    let char_id = 940_001_i64;
    let corp_id = 98_400_001_i64;
    let ally_id = 99_400_001_i64;
    let npc_corp_id = 1_000_035_i64;
    let agent_id = 3_018_999_i64;
    let struct_id = 1_045_900_001_i64;
    let sys_id = 30_000_142_u32;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.npc_corporations.insert(
        npc_corp_id,
        NpcCorporationInfo {
            corporation_id: npc_corp_id,
            name: "Caldari Navy".into(),
            ticker_name: "CN".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(sys_id),
            station_id: Some(60_003_760),
            tax_rate: 0.0,
            ceo_id: None,
        },
    );
    sde_mut.agents.insert(
        agent_id,
        AgentInfo {
            agent_id,
            name: "Agent Smith".into(),
            corporation_id: npc_corp_id,
            station_id: Some(60_003_760),
            solar_system_id: Some(sys_id),
            level: 4,
            division_id: 24,
            agent_type_id: 2,
            is_locator: true,
            research_skills: Vec::new(),
        },
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Player Corp".into();
                __corp.ticker = "PLRC".into();
                __corp.ceo_id = char_id;
                __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Ally".into(),
                short_name: "TALY".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Jita Hub".into(),
                position: [10.0, 20.0, 30.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        // Item at station, item at structure, and foreign hidden item in another system
        snap.items.insert(
            890_101,
            InventoryItemRecord {
                item_id: 890_101,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            890_102,
            InventoryItemRecord {
                item_id: 890_102,
                type_id: 587,
                owner_id: corp_id,
                location_id: struct_id,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            890_103,
            InventoryItemRecord {
                item_id: 890_103,
                type_id: 587,
                owner_id: 999_888_777,
                location_id: 30_000_144,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = auth_session(char_id, corp_id, Some(sys_id));
    let kw = HashMap::new();

    // GetMultiOwnersEx: resolves npc_corp, faction (500001), agent, and kwargs ownerIDs
    let faction_id = *ctx.sde.factions.keys().next().unwrap_or(&500_001) as i64;
    let kw_owners = HashMap::from([(
        "ownerIDs".to_string(),
        PyRep::Tuple(vec![
            PyRep::Integer(npc_corp_id),
            PyRep::Integer(faction_id),
            PyRep::Integer(agent_id),
        ]),
    )]);
    let owners = handle(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[], &kw_owners)
        .unwrap()
        .unwrap();
    assert_eq!(owners.row_count(), Some(3));

    // Single integer arg to parse_id_list vs invalid Dict arg
    let single_owner = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiOwnersEx",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(single_owner.row_count(), Some(1));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "config",
            "GetMultiOwnersEx",
            &[PyRep::Dict(vec![])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // GetLocationsEx: resolves region, constellation, solar system, stargate, structure, items (890101, 890102 visible; 890103 hidden)
    let reg_id = *ctx.sde.regions.keys().next().unwrap() as i64;
    let con_id = *ctx.sde.constellations.keys().next().unwrap() as i64;
    let gate_id = *ctx.sde.jumps.keys().next().unwrap();
    let locs = handle(
        &ctx,
        &mut sess,
        "config",
        "GetLocationsEx",
        &[PyRep::List(vec![
            PyRep::Integer(reg_id),
            PyRep::Integer(con_id),
            PyRep::Integer(sys_id as i64),
            PyRep::Integer(gate_id),
            PyRep::Integer(struct_id),
            PyRep::Integer(890_101),
            PyRep::Integer(890_102),
            PyRep::Integer(890_103), // hidden foreign item
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(locs.row_count(), Some(7));

    // GetMultiCorpTickerNamesEx: resolves both player corp and NPC corp
    let tickers = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiCorpTickerNamesEx",
        &[PyRep::List(vec![
            PyRep::Integer(corp_id),
            PyRep::Integer(npc_corp_id),
            PyRep::Integer(corp_id),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(tickers.row_count(), Some(2));

    // GetStationSolarSystemsByOwner: missing arg -> InvalidArgument, kwargs ownerID -> includes player structure
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "config",
            "GetStationSolarSystemsByOwner",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let kw_owner = HashMap::from([("ownerID".to_string(), PyRep::Integer(corp_id))]);
    let st_by_owner = handle(
        &ctx,
        &mut sess,
        "config",
        "GetStationSolarSystemsByOwner",
        &[],
        &kw_owner,
    )
    .unwrap()
    .unwrap();
    assert_eq!(st_by_owner.row_count(), Some(1));
}

#[test]
fn map_queries_and_notification_mgr_error_and_bulk_paths() {
    let ctx = test_ctx();
    let cid = 940_002_i64;
    let corp_id = 98_400_002_i64;
    let kw = HashMap::new();
    let mut sess = auth_session(cid, corp_id, None);

    // GetMap, get_map_objects, get_map_connections without solarSystemID or args -> InvalidArgument
    assert_eq!(
        handle(&ctx, &mut sess, "config", "GetMap", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "config",
            "GetMap",
            &[PyRep::Integer(99_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SolarSystemNotFound"
    );
    assert_eq!(
        get_map_objects(&ctx, &sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        get_map_connections(&ctx, &sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );

    // Entity 9 (universe), region, constellation, and explicit flags on get_map_objects & get_map_connections
    let reg_id = *ctx.sde.regions.keys().next().unwrap() as i64;
    let con_id = *ctx.sde.constellations.keys().next().unwrap() as i64;
    assert!(get_map_objects(&ctx, &sess, &[PyRep::Integer(9)])
        .unwrap()
        .row_count()
        .unwrap_or(0)
        > 0);
    assert!(get_map_objects(&ctx, &sess, &[PyRep::Integer(reg_id)])
        .unwrap()
        .row_count()
        .unwrap_or(0)
        > 0);
    assert!(get_map_objects(&ctx, &sess, &[PyRep::Integer(con_id)])
        .unwrap()
        .row_count()
        .unwrap_or(0)
        > 0);
    assert!(get_map_objects(
        &ctx,
        &sess,
        &[
            PyRep::Integer(30_000_142),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
    )
    .unwrap()
    .row_count()
    .unwrap_or(0)
        > 0);

    assert!(get_map_connections(&ctx, &sess, &[PyRep::Integer(9)])
        .unwrap()
        .row_count()
        .unwrap_or(0)
        > 0);
    assert!(get_map_connections(&ctx, &sess, &[PyRep::Integer(reg_id)])
        .unwrap()
        .row_count()
        .is_some());
    assert!(get_map_connections(&ctx, &sess, &[PyRep::Integer(con_id)])
        .unwrap()
        .row_count()
        .is_some());
    assert_eq!(
        get_map_connections(&ctx, &sess, &[PyRep::Integer(88_888_888)])
            .unwrap()
            .row_count(),
        Some(0)
    );
    assert_eq!(
        get_map_connections(
            &ctx,
            &sess,
            &[
                PyRep::Integer(9),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .unwrap()
        .row_count(),
        Some(0)
    );

    // notificationMgr: missing groupID errors & MarkAllAsProcessed
    let call_notif = |s: &mut SessionState, method: &str, args: &[PyRep]| {
        handle(&ctx, s, "notificationMgr", method, args, &kw).unwrap()
    };
    for m in ["GetByGroupID", "MarkGroupAsProcessed", "DeleteGroupNotifications"] {
        assert_eq!(call_notif(&mut sess, m, &[]).unwrap_err().code, "InvalidArgument");
    }
    ctx.notification_records.write().unwrap().push(NotificationRecord {
        notification_id: 9501,
        type_id: 1,
        group_id: 10,
        sender_id: 1_000_125,
        receiver_id: cid,
        created: 100,
        data: "hello".into(),
        processed: false,
        deleted: false,
        interacted: false,
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "notificationMgr",
            "MarkAllAsProcessed",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "notificationMgr",
            "MarkAllAsProcessed",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
}

#[test]
fn query_celestials_batch_get_locations_ex_and_unknown_service_mutants() {
    let ctx = test_ctx();
    let mut sess = auth_session(940_001, 98_400_001, Some(30_000_142));
    let kw = HashMap::new();

    // Kills `handle -> Some(Ok(Default::default()))`
    assert!(handle(&ctx, &mut sess, "nonexistent_svc", "AnyMethod", &[], &kw).is_none());

    // Pick a real SDE celestial (e.g. star or planet in Jita 30_000_142) not shadowed by system/station/gate IDs
    let cel = ctx
        .sde
        .celestials_in_system(30_000_142)
        .into_iter()
        .find(|c| matches!(c.group_id, 6 | 7 | 8))
        .expect("canonical seed Jita should contain star/planet celestials");

    // Genuine celestial ID resolves 1 row with its exact SDE name
    let cel_res = handle(
        &ctx,
        &mut sess,
        "config",
        "GetLocationsEx",
        &[PyRep::Integer(cel.celestial_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(cel_res.row_count(), Some(1));
    assert!(format!("{cel_res:?}").contains(&cel.name));

    // Sentinel IDs (0, 1, -1) must NOT resolve as phantom Default celestials; kills `[(0/1/-1, Default::default())]`
    for phantom_id in [0_i64, 1, -1] {
        let res = handle(
            &ctx,
            &mut sess,
            "config",
            "GetLocationsEx",
            &[PyRep::Integer(phantom_id)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(res.row_count(), Some(0), "id {phantom_id} should not resolve");
    }

    // Direct GetMultiOwnersEx assertion (kills delete match arm + handle -> None)
    let owners = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiOwnersEx",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(owners.row_count(), Some(1));
    assert!(format!("{owners:?}").contains("EVE System"));
}
