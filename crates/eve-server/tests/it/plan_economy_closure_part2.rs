//! Integration tests closing canonical plan row E-6
//! and its blocking planetary interaction lifecycle features.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{CorporationRecord, IskCents, PlanetaryColonyRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::*;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const P1: i64 = 90_000_001;
const POCO_CORP_ID: i64 = 2002;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn seed_extractor_colony(
    snap: &mut eve_db::WorldStateSnapshot,
    planet_id: i64,
    ecu_pin_id: i64,
    store_pin_id: i64,
    now_u: u64,
    expiry_unix: Option<u64>,
) {
    snap.planetary_colonies.insert(
        (P1, planet_id),
        PlanetaryColonyRecord {
            planet_id,
            owner_id: P1,
            solar_system_id: JITA_SYSTEM_ID,
            command_center_level: 3,
            pins_count: 2,
            last_extracted_unix: now_u,
            extracted_commodities: HashMap::new(),
        },
    );
    let net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: ecu_pin_id,
                type_id: 2848, // Extractor Control Unit
                latitude: 0.1,
                longitude: 0.1,
                extractor_resource_type_id: Some(2268), // Aqueous Liquids (0.01 m3/unit)
                extractor_head_count: 4,
                extractor_head_radius: 0.03,
                extractor_cycle_seconds: 900,
                program_install_unix: now_u,
                expiry_time_unix: expiry_unix,
                ..Default::default()
            },
            PiPinState {
                pin_id: store_pin_id,
                type_id: 2536, // Storage Facility (12,000 m3 capacity)
                latitude: 0.2,
                longitude: 0.2,
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: ecu_pin_id,
            dest_pin_id: store_pin_id,
            level: 1,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: 1,
            commodity_type_id: 2268,
            quantity: 100,
            path: vec![ecu_pin_id, store_pin_id],
        }],
        last_simulated_unix: now_u,
        ..Default::default()
    };
    save_pi_network(snap, P1, planet_id, &net);
}

#[tokio::test]
async fn test_e6_pi_extractor_expiry_and_launch_canister() {
    // parity: plan:E-6, feat:lifecycle.pi-extractor-program-expiry, feat:lifecycle.pi-launch-canister
    eve_testkit::attest(&[
        "plan:E-6",
        "feat:lifecycle.pi-extractor-program-expiry",
        "feat:lifecycle.pi-launch-canister",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let planet_single = 40_009_082i64; // Jita IV (single-call colony)
    let planet_multi = 40_009_083i64;  // Jita V (repeated-call colony)
    let ecu_pin_id = 50_000_001i64;
    let store_pin_id = 50_000_002i64;
    let now_u = ctx.now_unix();

    // 1. Seed two identical colonies for RPC-level repeated-call invariance (GetColony / StartTicking)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents(50_000_000_00);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        let mut poco_corp = CorporationRecord {
            corporation_id: POCO_CORP_ID,
            name: "Orbital Customs Corp".into(),
            ticker: "POCO".into(),
            ceo_id: 90_000_002,
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents(1_000_000_00),
            wallet_divisions: Vec::new(),
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(JITA_STATION_ID),
            members: [90_000_002].into_iter().collect(),
            member_roles: HashMap::new(),
            applications: HashMap::new(),
        };
        poco_corp.ensure_wallet_divisions();
        poco_corp.wallet_divisions[0].balance = IskCents(1_000_000_00);
        snap.corporations.insert(POCO_CORP_ID, poco_corp);
        save_orbital_settings(
            snap,
            planet_single,
            &OrbitalCustomsSettings {
                planet_id: planet_single,
                owner_corp_id: POCO_CORP_ID,
                corporation_tax_rate: 0.08,
                alliance_tax_rate: 0.08,
                standing_neutral_tax_rate: 0.12,
                allow_alliance: true,
                allow_standings: true,
                minimum_standing: -10.0,
                ..Default::default()
            },
        );
        seed_extractor_colony(snap, planet_single, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
        seed_extractor_colony(snap, planet_multi, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
    });

    let mut sess = SessionState::new(8501);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SYSTEM_ID);

    // 1a. Drive planet_multi through 16 sub-cycle steps of 450s via planetMgr.StartTicking & planetMgr.GetColony RPCs,
    // then drive planet_single through a single 7,200s planetMgr.GetColony RPC call at the end.
    for step in 1..=16 {
        ctx.advance_secs(450);
        if step % 2 == 1 {
            rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_multi)])
                .await
                .expect("planetMgr.StartTicking");
        } else {
            rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
                .await
                .expect("planetMgr.GetColony");
        }
    }
    let _ = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
        .await
        .expect("GetColony multi final");
    let single_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony single 7,200s");
    assert!(
        single_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
        "Single 7,200s GetColony call reports positive simulatedExtractedQuantity"
    );

    let (single_yield, multi_yield) = ctx.db.with_snapshot(|snap| {
        let net_s = load_pi_network(snap, P1, planet_single, 3);
        let net_m = load_pi_network(snap, P1, planet_multi, 3);
        (
            *net_s.pins[1].contents.get(&2268).unwrap_or(&0),
            *net_m.pins[1].contents.get(&2268).unwrap_or(&0),
        )
    });
    assert!(single_yield > 0, "Extractor produced yield");
    assert_eq!(
        single_yield, multi_yield,
        "RPC-level yield invariance: 16 StartTicking/GetColony sub-cycle calls == 1 single 7,200s GetColony call"
    );

    // Repeated GetColony/StartTicking at the same timestamp (0s elapsed) produces 0 extra yield
    let zero_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony with 0s elapsed");
    assert_eq!(
        zero_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()),
        Some(0),
        "Repeated GetColony with 0s elapsed yields 0"
    );

    // 1b. Extractor stops producing at install + duration (expiry); Flush after expiry never mints commodities
    ctx.advance_secs(10_000);
    rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_single)])
        .await
        .expect("StartTicking past expiry");
    let post_expiry_yield = ctx.db.with_snapshot(|snap| {
        *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
    });
    assert_eq!(single_yield, post_expiry_yield, "Extractor stops producing past expiry");

    ctx.advance_secs(900);
    let flush_expired = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
        .await
        .expect("Flush after extractor expiry");
    assert_eq!(
        flush_expired.dict_get("producedQuantity").and_then(|v| v.as_i64()),
        Some(0),
        "Flush must produce 0 when all extractor programs on the planet have expired"
    );

    // 1c. Bounded program duration on UserUpdateNetwork command 10 (InstallProgram):
    // Default (no client duration) -> 86,400s (24h); excessive client duration (99,999,999s) -> clamped to 14 * 86,400s (1,209,600s)
    let install_now = ctx.now_unix();
    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Real(0.03),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork command 10 default duration");
    let default_exp = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
    });
    assert_eq!(
        default_exp,
        Some(install_now + 86_400),
        "Extractor program installed without explicit duration defaults to bounded 24h (86,400s)"
    );

    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Real(0.03),
                PyRep::Integer(4),
                PyRep::Integer(900),
                PyRep::Integer((install_now + 99_999_999) as i64),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork command 10 clamped duration");
    let clamped_exp = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
    });
    assert_eq!(
        clamped_exp,
        Some(install_now + 14 * 86_400),
        "Client-supplied extractor expiry is clamped to 14 days (1,209,600s) max"
    );

    // Calling Flush immediately after install (before 1 cycle elapses) is rejected with ExtractorCycleNotReady
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
            col.last_extracted_unix = install_now;
        }
    });
    let flush_early_err = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
        .await
        .expect_err("Flush before cycle elapses must fail");
    assert_eq!(flush_early_err.code, "ExtractorCycleNotReady");

    // 2. Active extractor output beyond storage capacity is lost AND offline owner receives
    // PlanetExtractorFinished + PlanetStorageFull notifications on next login (SelectCharacterID)
    let offline_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
            col.last_extracted_unix = offline_now;
        }
        let mut net = load_pi_network(snap, P1, planet_single, 3);
        net.pins[0].program_install_unix = offline_now;
        net.pins[0].extractor_cycle_seconds = 900;
        net.pins[0].expiry_time_unix = Some(offline_now + 1_800);
        // Storage Facility (2536) has capacity 12,000.0 m3; Aqueous Liquids (2268) is 0.01 m3/unit -> max 1,200,000 units.
        // Pre-fill with 1,199,900 units so the first 900s cycle fills storage to 1,200,000 and overflows.
        net.pins[1].contents.clear();
        net.pins[1].contents.insert(2268, 1_199_900);
        net.last_simulated_unix = offline_now;
        save_pi_network(snap, P1, planet_single, &net);
    });

    let _ = ctx.drain_notifications_for_char(P1);
    // Advance 3,600s while P1 is offline (extractor finishes at +1,800s and storage fills to capacity)
    ctx.advance_secs(3_600);

    let mut sess_relogin = SessionState::new(8502);
    common::login_pilot(&ctx, &mut sess_relogin, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_relogin, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID after offline PI events");
    sess_relogin.solar_system_id = Some(JITA_SYSTEM_ID);

    let login_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        login_notifs.iter().any(|n| n.event == "PlanetExtractorFinished"),
        "Offline owner receives PlanetExtractorFinished notification at login"
    );
    assert!(
        login_notifs.iter().any(|n| n.event == "PlanetStorageFull"),
        "Offline owner receives PlanetStorageFull notification at login"
    );
    let persisted_types: Vec<i32> = ctx
        .notification_records
        .read()
        .unwrap()
        .iter()
        .filter(|r| r.receiver_id == P1)
        .map(|r| r.type_id)
        .collect();
    assert!(
        persisted_types.contains(&101) && persisted_types.contains(&102),
        "Both PlanetExtractorFinished (101) and PlanetStorageFull (102) persisted in notification_records"
    );

    // Trigger GetColony after relogin: verify storage clamped at 1,200,000 units and NO duplicate 101/102 notifications emitted
    let _ = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony after relogin");
    let post_get_colony_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        !post_get_colony_notifs
            .iter()
            .any(|n| n.event == "PlanetExtractorFinished" || n.event == "PlanetStorageFull"),
        "GetColony after login sync must not re-fire duplicate PlanetExtractorFinished or PlanetStorageFull notifications"
    );
    let capped_qty = ctx.db.with_snapshot(|snap| {
        *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
    });
    assert_eq!(
        capped_qty, 1_200_000,
        "Active extractor output beyond storage capacity (12,000 m3 = 1,200,000 units) is lost"
    );

    // 2b. Processor schematic (3,000 Aqueous Liquids P0 -> 20 Water P1) and expedited transfer cooldown
    let proc_client_id = 50_000_003i64;
    let lp_client_id = 50_000_004i64;
    rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(proc_client_id), PyRep::Integer(2473), PyRep::Real(0.25), PyRep::Real(0.25)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(lp_client_id), PyRep::Integer(2544), PyRep::Real(0.30), PyRep::Real(0.30)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_pin_id), PyRep::Integer(proc_client_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(proc_client_id), PyRep::Integer(lp_client_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(proc_client_id), PyRep::Integer(121)]),
            ]),
        ],
    )
    .await
    .expect("UserUpdateNetwork add processor, launchpad, links, and Water schematic 121");
    let (proc_pin_id, lp_pin_id) = ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, P1, planet_single, 3);
        let p_id = net.pins.iter().find(|p| p.type_id == 2473).unwrap().pin_id;
        let l_id = net.pins.iter().find(|p| p.type_id == 2544).unwrap().pin_id;
        (p_id, l_id)
    });
    rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(500))]),
        ],
    )
    .await
    .expect("UserTransferCommodities expedited transfer");
    let cd_err = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(100))]),
        ],
    )
    .await
    .expect_err("Expedited transfer within cooldown must fail");
    assert_eq!(cd_err.code, "ExpeditedTransferOnCooldown");

    // 3. POCO export tax charged & journaled on CustomsOffice export, and Launch Canister lifecycle (DeleteLaunch + 5-day expiry)
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = load_pi_network(snap, P1, planet_single, 3);
        net.pins[1].contents.clear();
        net.pins[1].contents.insert(3645, 1_000); // 1,000 units of Water (P1 commodity, base 400 ISK)
        save_pi_network(snap, P1, planet_single, &net);
    });

    let p1_bal_before_poco = ctx.db.get_character(P1).unwrap().balance.0;
    let poco_corp_before = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet.0);
    let poco_export_rep = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::Integer(3645),
            PyRep::Integer(200),
            PyRep::String("CustomsOffice".into()),
        ],
    )
    .await
    .expect("UserLaunchCommodities via CustomsOffice");
    let export_tax_isk = poco_export_rep
        .dict_get("exportTaxISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(export_tax_isk > 0.0, "POCO export must charge positive exportTaxISK");
    let p1_bal_after_poco = ctx.db.get_character(P1).unwrap().balance.0;
    let poco_corp_after = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet.0);
    assert!(p1_bal_after_poco < p1_bal_before_poco, "P1 wallet debited by POCO export tax");
    assert!(poco_corp_after > poco_corp_before, "POCO owner corporation credited with player tax share");
    let poco_launch_id = poco_export_rep.dict_get("launchID").and_then(|v| v.as_i64()).expect("poco_launch_id");

    // DeleteLaunch on a REAL existing launch ID succeeds and removes the launch canister and its ball
    let del_ok = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(poco_launch_id)])
        .await
        .expect("DeleteLaunch on real launch canister must succeed");
    assert_eq!(del_ok, PyRep::Bool(true));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&poco_launch_id)),
        "DeleteLaunch removes the launch canister item from space"
    );

    // DeleteLaunch on a non-launch item ID is rejected with InvalidLaunchID
    let del_err = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(100_000_001)])
        .await
        .expect_err("DeleteLaunch must reject non-launch ID");
    assert_eq!(del_err.code, "InvalidLaunchID");

    // Command Center launch creates a real Planetary Launch Container item and Ball in space near the planet
    // (never in station hangar), places commodities inside the container, preserves launchTime, and expires after 5 days
    let launch_ts = ctx.now_unix() as i64;
    let launch_rep = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::Integer(store_pin_id),
            PyRep::Integer(3645),
            PyRep::Integer(500),
        ],
    )
    .await
    .expect("UserLaunchCommodities CommandCenterLaunch");
    let launch_id = launch_rep
        .as_i64()
        .or_else(|| launch_rep.dict_get("launchID").and_then(|v| v.as_i64()))
        .expect("launch_id");
    assert!(launch_id > 0);
    let (container_item, inner_commodity) = ctx.db.with_snapshot(|s| {
        let cont = s.items.get(&launch_id).cloned().expect("Planetary Launch Container item");
        let inner = s
            .items
            .values()
            .find(|i| i.location_id == launch_id)
            .cloned()
            .expect("Commodity item inside launch container");
        (cont, inner)
    });
    assert_eq!(
        container_item.location_id,
        JITA_SYSTEM_ID as i64,
        "Planetary Launch Container located in solar system space"
    );
    assert_ne!(
        container_item.type_id, 3645,
        "Launch item is a Planetary Launch Container SDE type, not the raw commodity"
    );
    assert_eq!(inner_commodity.type_id, 3645);
    assert_eq!(inner_commodity.quantity, 500);
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let (_, launch_ball) = sys_handle
        .get_ball_snapshot(launch_id)
        .await
        .expect("ball query")
        .expect("Planetary Launch Container ball spawned in ballpark");
    assert_eq!(launch_ball.type_id, container_item.type_id);
    assert!(
        launch_ball.position.length() > 1_000.0,
        "Launch container ball has non-zero orbital coordinates near planet"
    );
    let in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.location_id == JITA_STATION_ID && i.type_id == 3645)
    });
    assert!(!in_hangar, "Never places launched goods in station hangar");

    // Advance 60s and verify GetMyLaunchesDetails reports the stored launchTime and non-zero (x, y, z)
    ctx.advance_secs(60);
    let my_launches = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(ref launch_list) = my_launches else {
        panic!("GetMyLaunchesDetails must return List");
    };
    let found_launch = launch_list
        .iter()
        .find(|entry| entry.dict_get("launchID").and_then(|v| v.as_i64()) == Some(launch_id))
        .expect("launch entry in GetMyLaunchesDetails");
    assert_eq!(
        found_launch.dict_get("launchTime").and_then(|v| v.as_i64()),
        Some(launch_ts),
        "GetMyLaunchesDetails reports stored launchTime"
    );
    let lx = found_launch.dict_get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let ly = found_launch.dict_get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let lz = found_launch.dict_get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(
        lx.abs() > 1.0 && ly.abs() > 1.0 && lz.abs() > 1.0,
        "GetMyLaunchesDetails reports non-zero orbital coordinates (x, y, z)"
    );

    // Uncollected after 5 days is removed by background tick (before GetMyLaunchesDetails is called)
    ctx.advance_secs(5 * 86_400 + 100);
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&launch_id) && !s.items.contains_key(&inner_commodity.item_id)),
        "Background tick removes expired planetary launch container and its inner commodities after 5 days"
    );
    assert!(
        sys_handle.get_ball_snapshot(launch_id).await.expect("ball query").is_none(),
        "Background tick removes expired planetary launch container ball from ballpark"
    );
    let launches_expired = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetMyLaunchesDetails after 5 days");
    assert!(
        !format!("{launches_expired:?}").contains(&launch_id.to_string()),
        "Uncollected launch removed after 5 days"
    );
}
