//! Remediation integration tests for navigation features (part 1):
//! `feat:navigation.route-waypoints`, `feat:navigation.wormhole-statics`.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{CosmicSignatureRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::nav_warp_interdiction::calculate_route_with_waypoints;
use eve_server::services::wormhole_lifecycle_ops::{
    build_static_replacement_wormhole, lookup_wormhole_type_for_origin_and_target,
    system_wormhole_profile_sde, wspace_static_specs_for_system,
};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_navigation_route_waypoints() {
    // parity: feat:navigation.route-waypoints
    eve_testkit::attest(&["feat:navigation.route-waypoints"]);

    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    // Jita is 30000142, Amarr is 30002187, Perimeter is 30000144, Urlen is 30000143
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let perimeter = 30_000_144_u32;
    let urlen = 30_000_143_u32;

    // 1. Direct route calculation
    let direct_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[amarr as i64],
        &[],
        "shortest",
    )
    .expect("Direct Jita to Amarr route");
    assert!(!direct_route.is_empty());
    assert_eq!(*direct_route.last().unwrap(), amarr);

    // 2. Waypoint routing through Perimeter
    let wp_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[perimeter as i64, amarr as i64],
        &[],
        "shortest",
    )
    .expect("Route with Perimeter waypoint");
    assert!(wp_route.contains(&perimeter));
    assert_eq!(*wp_route.last().unwrap(), amarr);

    // 3. Avoid list - avoid Perimeter forces alternate route
    let avoided_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[amarr as i64],
        &[perimeter],
        "shortest",
    )
    .expect("Route avoiding Perimeter");
    assert!(!avoided_route.contains(&perimeter));

    // 4. Destination resolution: NPC station ID (Jita 4-4 station 60003760 resolves to Jita 30000142)
    let jita_station_id = 60_003_760_i64;
    let station_route = calculate_route_with_waypoints(
        &ctx,
        amarr,
        &[jita_station_id],
        &[],
        "shortest",
    )
    .expect("Route to Jita 4-4 station");
    assert_eq!(*station_route.last().unwrap(), jita);

    // 5. Unreachable waypoint (e.g. w-space system 31000001) returns WaypointUnreachable
    let wspace_sys = 31_000_001_i64;
    let unreach_err = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[wspace_sys, amarr as i64],
        &[],
        "shortest",
    )
    .expect_err("W-space waypoint should fail");
    assert_eq!(unreach_err.code, "WaypointUnreachable");

    // 6. ESI route query with waypoints and avoid params
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let esi_url =
        format!("/v1/route/{jita}/{amarr}/?waypoints={urlen}&avoid={perimeter}&flag=shortest");
    let esi_resp = esi.handle_request(&EsiHttpRequest::new("GET", &esi_url));
    assert_eq!(esi_resp.status_code, 200);
    let esi_body: Vec<u32> = serde_json::from_slice(&esi_resp.body).expect("Valid JSON route");
    assert!(esi_body.contains(&urlen));
    assert!(!esi_body.contains(&perimeter));
    assert_eq!(*esi_body.last().unwrap(), amarr);

    // 7. Multi-waypoint persistence in CharacterSettings across sessions
    let char_id = 90_000_001_i64;
    let mut sess = SessionState::new(20_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let wp_list = PyRep::List(vec![
        PyRep::Integer(perimeter as i64),
        PyRep::Integer(urlen as i64),
        PyRep::Integer(amarr as i64),
    ]);
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "SaveCharacterSetting",
        &[PyRep::String("waypoints".into()), wp_list.clone()],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SaveCharacterSetting");

    // Disconnect and reconnect with fresh session to verify persistence
    let mut sess2 = SessionState::new(20_102);
    common::login_pilot(&ctx, &mut sess2, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID sess2");
    let settings = ctx
        .dispatch_rpc(
            &mut sess2,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetCharacterSettings");
    assert_eq!(settings.dict_get("waypoints"), Some(&wp_list));
}

#[tokio::test]
async fn test_navigation_wormhole_statics() {
    // parity: feat:navigation.wormhole-statics
    eve_testkit::attest(&["feat:navigation.wormhole-statics"]);

    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let now_unix = ctx.now_unix();

    // 1. Authentic SDE statics across wormhole classes C1 through C6
    // C1: 1 static (K-space target class 7, 8, or 9)
    let c1_sys = 31_000_100_u32;
    let c1_specs = wspace_static_specs_for_system(&ctx.sde, c1_sys);
    assert_eq!(c1_specs.len(), 1);
    assert_eq!(c1_specs[0].0, "WST");
    assert!([7, 8, 9].contains(&c1_specs[0].3));
    let wh_info = ctx
        .sde
        .wormhole_types
        .get(&c1_specs[0].2)
        .expect("C1 static type in SDE");
    assert_eq!(wh_info.target_system_class, c1_specs[0].3);

    // C2: 2 statics (1 W-space + 1 K-space)
    let c2_sys = 31_000_400_u32;
    let c2_specs = wspace_static_specs_for_system(&ctx.sde, c2_sys);
    assert_eq!(c2_specs.len(), 2, "C2 must have dual statics");
    assert_eq!(c2_specs[0].0, "WST");
    assert_eq!(c2_specs[1].0, "WST2");
    assert!((1..=6).contains(&c2_specs[0].3), "C2 static 1 is W-space");
    assert!([7, 8, 9].contains(&c2_specs[1].3), "C2 static 2 is K-space");
    let c2_t0 = ctx
        .sde
        .wormhole_types
        .get(&c2_specs[0].2)
        .expect("C2 static 0 type in SDE");
    let c2_t1 = ctx
        .sde
        .wormhole_types
        .get(&c2_specs[1].2)
        .expect("C2 static 1 type in SDE");
    assert_eq!(c2_t0.target_system_class, c2_specs[0].3);
    assert_eq!(c2_t1.target_system_class, c2_specs[1].3);

    // C3: 1 static (K-space target class 7, 8, or 9)
    let c3_sys = 31_000_900_u32;
    let c3_specs = wspace_static_specs_for_system(&ctx.sde, c3_sys);
    assert_eq!(c3_specs.len(), 1);
    assert!([7, 8, 9].contains(&c3_specs[0].3));

    // C4: 2 distinct W-space statics
    let c4_sys = 31_001_500_u32;
    let c4_specs = wspace_static_specs_for_system(&ctx.sde, c4_sys);
    assert_eq!(
        c4_specs.len(),
        2,
        "C4 must have dual distinct W-space statics"
    );
    assert_ne!(
        c4_specs[0].3, c4_specs[1].3,
        "C4 dual statics must be distinct classes"
    );
    assert!((1..=6).contains(&c4_specs[0].3));
    assert!((1..=6).contains(&c4_specs[1].3));

    // C5 and C6: 1 W-space static
    let c5_sys = 31_002_000_u32;
    let c5_specs = wspace_static_specs_for_system(&ctx.sde, c5_sys);
    assert_eq!(c5_specs.len(), 1);
    assert!((1..=6).contains(&c5_specs[0].3));

    let c6_sys = 31_002_400_u32;
    let c6_specs = wspace_static_specs_for_system(&ctx.sde, c6_sys);
    assert_eq!(c6_specs.len(), 1);
    assert!((1..=6).contains(&c6_specs[0].3));

    // 2. Static replacement wormhole respawn on collapse
    let initial_sig = CosmicSignatureRecord {
        signature_id: "WST-101".into(),
        solar_system_id: c2_sys,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(c2_specs[0].2),
        destination_system_id: Some(31_000_123),
        created_unix: now_unix - 1000,
        lifetime_seconds: 86400,
        max_mass_kg: 1_000_000_000.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 300_000_000.0,
        collapsed: true,
    };
    let replacement = build_static_replacement_wormhole(&ctx, c2_sys, &initial_sig, now_unix, 1)
        .expect("Replacement static wormhole");
    assert!(replacement.signature_id.starts_with("WST-"));
    assert!(!replacement.collapsed);
    assert_eq!(replacement.remaining_mass_kg, replacement.max_mass_kg);
    assert!(replacement.remaining_mass_kg > 0.0);
    assert!(replacement.destination_system_id.is_some());
    assert_ne!(
        replacement.destination_system_id,
        initial_sig.destination_system_id
    );
    let dest_sys = replacement.destination_system_id.unwrap();
    let (dest_cls, _, _, _) = system_wormhole_profile_sde(&ctx.sde, dest_sys);
    let expected_target = ctx
        .sde
        .wormhole_types
        .get(&replacement.wormhole_type_id.unwrap())
        .unwrap()
        .target_system_class;
    assert_eq!(dest_cls as i32, expected_target);

    // 3. Wandering wormholes per class from SDE
    for origin_cls in 1..=6 {
        for target_cls in [1, 2, 7, 8, 9] {
            let wh_tid = lookup_wormhole_type_for_origin_and_target(
                &ctx.sde, origin_cls, target_cls, 42,
            );
            assert_ne!(wh_tid, 26_272, "Must not pick fallback K162 type");
            let wh = ctx
                .sde
                .wormhole_types
                .get(&wh_tid)
                .expect("Wandering wh type in SDE");
            assert_eq!(wh.target_system_class, target_cls);
            assert!(wh.max_jump_mass > 0.0);
        }
    }
}
