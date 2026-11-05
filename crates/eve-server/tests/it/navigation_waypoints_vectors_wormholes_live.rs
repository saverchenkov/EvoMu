//! Live Integration Suite for Navigation Route Waypoints, Warp Vector Data,
//! Wormhole Statics, and Wormhole Show-Info Buckets.
//!
//! Covers:
//! - `feat:navigation.route-waypoints`
//! - `feat:navigation.warp-vector`
//! - `feat:navigation.wormhole-statics`
//! - `feat:navigation.wormhole-info-buckets`

#![forbid(unsafe_code)]

use eve_destiny::MovementMode;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::derive_wormhole_buckets_raw;
use eve_server::services::wormhole_lifecycle_ops::wspace_static_specs_for_system;
use std::collections::HashMap;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn ctx_with_real_sde() -> ServerContext {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    crate::common::pin_clock(&ctx);
    ctx
}

fn drain_all(
    ctx: &ServerContext,
    sess: &mut eve_machonet::SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

// parity: feat:navigation.route-waypoints
#[tokio::test]
async fn test_navigation_route_waypoints_live() {
    eve_testkit::attest(&["feat:navigation.route-waypoints"]);

    let ctx = Arc::new(ctx_with_real_sde());

    let char_id = 91_700_001_i64;
    let p = crate::common::gm_pilot_in_space(&ctx, 1001, char_id, 606, 30_000_142).await;
    let mut sess = p.sess.clone();

    // Drain initial login notifications
    let _ = drain_all(&ctx, &mut sess, char_id);

    // Issue PKCE OAuth token for live pilot
    let now = ctx.now_unix();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    let code = ctx
        .db
        .authorize_pkce(
            &user,
            &pass,
            char_id,
            "evomu-nav-test",
            "https://localhost/callback",
            vec!["esi-ui.write_waypoint.v1".to_string()],
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(&code, "evomu-nav-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token;

    let esi = EsiRouter::new(Arc::clone(&ctx));

    // 1. Set destination to Amarr (30002187) with clear_other_waypoints=true
    let req1 = EsiHttpRequest::new(
        "POST",
        "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true",
    )
    .with_bearer_token(&token);
    let resp1 = esi.handle_request(&req1);
    assert_eq!(resp1.status_code, 204);

    let notifs1 = drain_all(&ctx, &mut sess, char_id);
    let change_notif = notifs1
        .iter()
        .find(|(ev, _)| ev == "OnDestinationChange")
        .expect("OnDestinationChange must be emitted on destination set");
    assert_eq!(
        change_notif.1,
        vec![
            PyRep::Integer(30_002_187),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ]
    );
    assert!(
        notifs1.iter().any(|(ev, _)| ev == "OnAutopilotUpdated"),
        "OnAutopilotUpdated must be emitted"
    );

    let settings = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetCharacterSettings");
    assert_eq!(
        settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(30_002_187)]))
    );

    // 2. Append Jita 4-4 station (60003760) with add_to_beginning=false
    let req2 = EsiHttpRequest::new(
        "POST",
        "/v2/ui/autopilot/waypoint/?destination_id=60003760&add_to_beginning=false&clear_other_waypoints=false",
    )
    .with_bearer_token(&token);
    let resp2 = esi.handle_request(&req2);
    assert_eq!(resp2.status_code, 204);

    let settings2 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetCharacterSettings");
    assert_eq!(
        settings2.dict_get("waypoints"),
        Some(&PyRep::List(vec![
            PyRep::Integer(30_002_187),
            PyRep::Integer(60_003_760),
        ]))
    );

    // 3. Prepend Urlen (30000143) with add_to_beginning=true
    let req3 = EsiHttpRequest::new(
        "POST",
        "/v2/ui/autopilot/waypoint/?destination_id=30000143&add_to_beginning=true&clear_other_waypoints=false",
    )
    .with_bearer_token(&token);
    let resp3 = esi.handle_request(&req3);
    assert_eq!(resp3.status_code, 204);

    let settings3 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetCharacterSettings");
    assert_eq!(
        settings3.dict_get("waypoints"),
        Some(&PyRep::List(vec![
            PyRep::Integer(30_000_143),
            PyRep::Integer(30_002_187),
            PyRep::Integer(60_003_760),
        ]))
    );

    // 4. Query route calculation from Jita to Amarr via ESI
    let route_req = EsiHttpRequest::new("GET", "/v1/route/30000142/30002187/?flag=shortest");
    let route_resp = esi.handle_request(&route_req);
    assert_eq!(route_resp.status_code, 200);
    assert!(!route_resp.body.is_empty());

    // 5. Clear waypoints via charMgr.DeleteCharacterSetting
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "DeleteCharacterSetting",
        &[PyRep::String("waypoints".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("DeleteCharacterSetting");

    let settings_cleared = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetCharacterSettings");
    assert!(
        settings_cleared.dict_get("waypoints").is_none(),
        "Waypoints setting must be cleared after DeleteCharacterSetting"
    );
}

// parity: feat:navigation.warp-vector
#[tokio::test]
async fn test_navigation_warp_vector_live() {
    eve_testkit::attest(&["feat:navigation.warp-vector"]);

    let ctx = Arc::new(ctx_with_real_sde());

    let char_id = 91_700_002_i64;
    let mut p = crate::common::gm_pilot_in_space(&ctx, 1002, char_id, 606, 30_000_142).await;
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);

    // Verify ship begins in space in non-warp mode
    let (_, initial_ball) = sys_handle
        .get_ball_snapshot(p.ship_id)
        .await
        .expect("destiny query")
        .expect("ship ball in space");
    assert!(!matches!(initial_ball.mode, MovementMode::Warp { .. }));

    // Drain initial notifications
    let _ = ctx.drain_notifications_for_char(char_id);

    // Create 2x Cargo Warp Vector Data (type 77825, group 4599) in active ship cargo via GM slash command
    let created = crate::common::slash_cmd(&ctx, &mut p.sess, "/createitem 77825 2").await;
    let item_id = created
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("created itemID");

    // Initiate warp to vector
    let res = ctx
        .dispatch_rpc(
            &mut p.sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("warpVectorMgr.UseVector");

    assert_eq!(res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let dist_m = res
        .dict_get("distanceMeters")
        .and_then(|v| v.as_f64())
        .expect("distanceMeters");
    assert!(dist_m >= 150_000.0, "Warp vector distance must be >= 150 km (got {dist_m})");
    let warp_spd = res
        .dict_get("warpSpeedAuPerSec")
        .and_then(|v| v.as_f64())
        .expect("warpSpeedAuPerSec");
    assert!(warp_spd > 0.0);

    // Verify OnWarpStarted notification
    let notifs = ctx.drain_notifications_for_char(char_id);
    let warp_notif = notifs
        .iter()
        .find(|n| n.event == "OnWarpStarted")
        .expect("OnWarpStarted must be emitted on warp vector activation");
    assert_eq!(warp_notif.args[0], PyRep::Integer(p.ship_id));

    // Verify authentic destiny ballistic trajectory: ball mode is MovementMode::Warp (not instant teleport)
    let (_, warp_ball) = sys_handle
        .get_ball_snapshot(p.ship_id)
        .await
        .expect("destiny query")
        .expect("ship ball in space");
    match warp_ball.mode {
        MovementMode::Warp { destination, warp_speed_au_s, .. } => {
            assert!(destination.is_finite(), "Warp destination vector must be finite");
            assert!((warp_speed_au_s - warp_spd).abs() < 1e-4);
        }
        ref other => panic!("Ship must be in MovementMode::Warp trajectory, got: {other:?}"),
    }

    // Verify 300s cooldown prevents immediate re-activation
    let err = ctx
        .dispatch_rpc(
            &mut p.sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Immediate re-use of warp vector must be rejected by cooldown");
    assert_eq!(err.code, "WarpVectorCooldown");
}

// parity: feat:navigation.wormhole-statics
#[tokio::test]
async fn test_navigation_wormhole_statics_live() {
    eve_testkit::attest(&["feat:navigation.wormhole-statics"]);

    let ctx = Arc::new(ctx_with_real_sde());

    // 1. Static specifications across wormhole classes C1..C6
    // C1: 1 static targeting K-space
    let c1_specs = wspace_static_specs_for_system(&ctx.sde, 31_000_100);
    assert_eq!(c1_specs.len(), 1);
    assert!([7, 8, 9].contains(&c1_specs[0].3));

    // C2: 2 statics (1 W-space + 1 K-space)
    let c2_specs = wspace_static_specs_for_system(&ctx.sde, 31_000_400);
    assert_eq!(c2_specs.len(), 2, "C2 must have dual statics (W-space and K-space)");
    assert_eq!(c2_specs[0].0, "WST");
    assert_eq!(c2_specs[1].0, "WST2");
    assert!((1..=6).contains(&c2_specs[0].3));
    assert!([7, 8, 9].contains(&c2_specs[1].3));

    // C3: 1 static targeting K-space
    let c3_specs = wspace_static_specs_for_system(&ctx.sde, 31_000_900);
    assert_eq!(c3_specs.len(), 1);
    assert!([7, 8, 9].contains(&c3_specs[0].3));

    // C4: 2 distinct W-space statics
    let c4_specs = wspace_static_specs_for_system(&ctx.sde, 31_001_500);
    assert_eq!(c4_specs.len(), 2, "C4 must have dual distinct W-space statics");
    assert_ne!(c4_specs[0].3, c4_specs[1].3);
    assert!((1..=6).contains(&c4_specs[0].3));
    assert!((1..=6).contains(&c4_specs[1].3));

    // C5 & C6: 1 static targeting W-space
    let c5_specs = wspace_static_specs_for_system(&ctx.sde, 31_002_000);
    assert_eq!(c5_specs.len(), 1);
    assert!((1..=6).contains(&c5_specs[0].3));

    let c6_specs = wspace_static_specs_for_system(&ctx.sde, 31_002_400);
    assert_eq!(c6_specs.len(), 1);
    assert!((1..=6).contains(&c6_specs[0].3));

    // 2. Query live C2 system signatures via scanMgr.GetSystemScanMgr
    let c2_sys = 31_000_400_u32;
    let char_id = 91_700_003_i64;
    let mut p = crate::common::gm_pilot_in_space(&ctx, 1003, char_id, 606, c2_sys).await;
    p.sess.solar_system_id = Some(c2_sys);

    let scan_rep = ctx
        .dispatch_rpc(
            &mut p.sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(c2_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("scanMgr.GetSystemScanMgr");

    let sigs = scan_rep
        .dict_get("signatures")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.clone()),
            _ => None,
        })
        .expect("signatures list");


    let wst_sig = sigs
        .iter()
        .find(|s| {
            s.dict_get("signatureID")
                .and_then(|v| match v {
                    PyRep::String(str) => Some(str.starts_with("WST-")),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .expect("WST- static signature in C2 system");
    let wst_sig_id = match wst_sig.dict_get("signatureID").unwrap() {
        PyRep::String(s) => s.clone(),
        _ => panic!(),
    };

    let wst2_sig = sigs
        .iter()
        .find(|s| {
            s.dict_get("signatureID")
                .and_then(|v| match v {
                    PyRep::String(str) => Some(str.starts_with("WST2-")),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .expect("WST2- static signature in C2 system");
    assert!(wst2_sig.dict_get("signatureID").is_some());

    // 3. Static replacement wormhole respawns automatically on collapse
    ctx.advance_secs(86_401); // Advance past 24h lifetime to expire the static
    let post_collapse_scan = ctx
        .dispatch_rpc(
            &mut p.sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(c2_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("scanMgr.GetSystemScanMgr after collapse");

    let post_sigs = post_collapse_scan
        .dict_get("signatures")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.clone()),
            _ => None,
        })
        .expect("post signatures list");

    // The old static signature must be replaced with a fresh replacement static
    let respawned_wst = post_sigs
        .iter()
        .find(|s| {
            s.dict_get("signatureID")
                .and_then(|v| match v {
                    PyRep::String(str) => Some(str.starts_with("WST-")),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .expect("Fresh replacement WST static must be seeded");

    let new_sig_id = match respawned_wst.dict_get("signatureID").unwrap() {
        PyRep::String(s) => s.clone(),
        _ => panic!(),
    };
    assert_ne!(new_sig_id, wst_sig_id, "Replacement static must receive a new signature ID");
}

// parity: feat:navigation.wormhole-info-buckets
#[tokio::test]
async fn test_navigation_wormhole_info_buckets_live() {
    eve_testkit::attest(&["feat:navigation.wormhole-info-buckets"]);

    let ctx = Arc::new(ctx_with_real_sde());
    let now = ctx.now_unix();

    // 1. Lifetime decay stages (> 24h, 4h-24h, < 4h, < 1h, expired) and authentic CCP show-info strings
    // Fresh (> 24h remaining, e.g. 36h)
    let fresh = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 48 * 3_600, false, now + 12 * 3_600, &ctx.sde,
    );
    assert_eq!(fresh.lifetime_bucket, ">1 day");
    assert_eq!(
        fresh.lifetime_description,
        "This wormhole has not yet begun its natural cycle of decay and is not likely to disappear for some time"
    );

    // Decaying (4h-24h remaining, e.g. 14h)
    let decaying = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now + 10 * 3_600, &ctx.sde,
    );
    assert_eq!(decaying.lifetime_bucket, "<1 day");
    assert_eq!(
        decaying.lifetime_description,
        "This wormhole is beginning to decay, and probably won't last another day"
    );

    // Critical (< 4h remaining, e.g. 2h)
    let critical = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now + 22 * 3_600, &ctx.sde,
    );
    assert_eq!(critical.lifetime_bucket, "<4h");
    assert_eq!(
        critical.lifetime_description,
        "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon"
    );

    // Imminent (< 1h remaining, e.g. 30 min)
    let imminent = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now + 23 * 3_600 + 1_800, &ctx.sde,
    );
    assert_eq!(imminent.lifetime_bucket, "<1h");

    // Expired
    let expired = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now + 25 * 3_600, &ctx.sde,
    );
    assert_eq!(expired.lifetime_bucket, "expired");

    // 2. Mass stability stage buckets (> 50%, < 50%, < 10%) and authentic CCP show-info strings
    // Stable (> 50%)
    let mass_stable = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 1_800_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(mass_stable.mass_stability_bucket, ">50%");
    assert_eq!(
        mass_stable.mass_stability_description,
        "This wormhole has not yet had its stability significantly disrupted by ships passing through it."
    );

    // Destabilized (< 50%, >= 10%)
    let mass_destab = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 600_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(mass_destab.mass_stability_bucket, "<50%");
    assert_eq!(
        mass_destab.mass_stability_description,
        "This wormhole has had its stability reduced by ships passing through it, but not to a critical degree yet."
    );

    // Critical (< 10%)
    let mass_crit = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 100_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(mass_crit.mass_stability_bucket, "<10%");
    assert_eq!(
        mass_crit.mass_stability_description,
        "This wormhole has had its stability critically disrupted by the mass of numerous ships passing through and is on the verge of collapse."
    );

    // 3. Max ship size categories across real SDE wormhole types
    // Small (<= 20M kg)
    let sz_small = derive_wormhole_buckets_raw(
        30_000_142, 20_000_000.0, 500_000_000.0, 500_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(sz_small.max_ship_size, "Small");

    // Medium (<= 62M kg)
    let sz_medium = derive_wormhole_buckets_raw(
        30_000_142, 62_000_000.0, 1_000_000_000.0, 1_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(sz_medium.max_ship_size, "Medium");

    // Large (<= 500M kg, e.g. B041 / B274 at 375M kg)
    let sz_large = derive_wormhole_buckets_raw(
        30_000_142, 375_000_000.0, 2_000_000_000.0, 2_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(sz_large.max_ship_size, "Large");

    // Extra Large (> 500M kg, e.g. capital wormholes at 1B or 2B kg)
    let sz_xl = derive_wormhole_buckets_raw(
        30_000_142, 1_000_000_000.0, 3_000_000_000.0, 3_000_000_000.0,
        now, 24 * 3_600, false, now, &ctx.sde,
    );
    assert_eq!(sz_xl.max_ship_size, "Extra Large");

    // 4. Live RPC verification via scanMgr.GetSystemScanMgr
    let c3_sys = 31_000_900_u32;
    let char_id = 91_700_004_i64;
    let mut p = crate::common::gm_pilot_in_space(&ctx, 1004, char_id, 606, c3_sys).await;
    p.sess.solar_system_id = Some(c3_sys);

    let initial_scan = ctx
        .dispatch_rpc(
            &mut p.sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(c3_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("scanMgr.GetSystemScanMgr");

    let sigs = initial_scan
        .dict_get("signatures")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.clone()),
            _ => None,
        })
        .expect("signatures list");

    let wh_sig = sigs
        .iter()
        .find(|s| {
            s.dict_get("signatureID")
                .and_then(|v| match v {
                    PyRep::String(str) => Some(str.starts_with("WST-")),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .expect("WST signature in C3 system");
    let wh_sig_id = match wh_sig.dict_get("signatureID").unwrap() {
        PyRep::String(s) => s.clone(),
        _ => panic!(),
    };

    // Record probe scan resolution for this pilot to reveal full show-info buckets
    ctx.record_character_scan_progress(char_id, c3_sys, &wh_sig_id, 1.0);

    let resolved_scan = ctx
        .dispatch_rpc(
            &mut p.sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(c3_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("resolved scanMgr.GetSystemScanMgr");

    let resolved_sigs = resolved_scan
        .dict_get("signatures")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.clone()),
            _ => None,
        })
        .expect("resolved signatures list");

    let resolved_wh = resolved_sigs
        .iter()
        .find(|s| s.dict_get("signatureID") == Some(&PyRep::String(wh_sig_id.clone())))
        .expect("resolved wormhole signature");

    assert!(resolved_wh.dict_get("lifetimeBucket").is_some());
    assert!(resolved_wh.dict_get("massStabilityBucket").is_some());
    assert!(resolved_wh.dict_get("maxShipSize").is_some());
    assert!(resolved_wh.dict_get("destinationClass").is_some());
    assert!(resolved_wh.dict_get("lifetimeDescription").is_some());
    assert!(resolved_wh.dict_get("massStabilityDescription").is_some());
}
