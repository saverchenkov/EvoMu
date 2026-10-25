//! Integration tests covering open RPC parity rows across station and corp station management services (Part 1).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;
const CORP_CALDARI_NAVY: i64 = 1_000_035;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_station_services_and_corp_station_mgr_parity() {
    // parity: rpc:corpStationMgr.DoStandingCheckForStationService, rpc:station.GetGuests, rpc:stationSvc.GetAllianceSystems, rpc:stationSvc.GetSolarSystem, rpc:stationSvc.GetStationItemBits, rpc:stationSvc.GetStationsForOwner, rpc:stationSvc.GetSystemsForAlliance
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", PILOT2).await;

    // Seed an offline character docked at STATION_JITA who has never logged in
    let offline_uid = ctx.db.provision_player_account("offline_guest", "offline-secret");
    let offline_cid = ctx
        .db
        .create_character(offline_uid, "Offline Docked Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create offline char")
        .character_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&offline_cid) {
            ch.station_id = Some(STATION_JITA);
        }
    });

    // Undock PILOT2 via stationSvc.Undock so PILOT1 is docked at STATION_JITA and PILOT2 is undocked in space
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[])
        .await
        .expect("stationSvc.Undock PILOT2");

    // 1. station.GetGuests: returns online docked PILOT1, excludes undocked PILOT2 and offline docked pilot
    let guests_single = rpc(&ctx, &mut s1, "station", "GetGuests", &[])
        .await
        .expect("station.GetGuests single");
    let PyRep::List(guest_list_1) = guests_single else {
        panic!("station.GetGuests must return a List");
    };
    let mut cids_1 = Vec::new();
    for g in &guest_list_1 {
        let PyRep::Tuple(t) = g else {
            panic!("Guest entry must be a Tuple");
        };
        assert_eq!(t.len(), 4, "Guest tuple must have 4 elements (char, corp, ally, fac)");
        cids_1.push(t[0].as_i64().expect("characterID"));
    }
    assert!(cids_1.contains(&PILOT1), "PILOT1 must be in guest list");
    assert!(!cids_1.contains(&PILOT2), "PILOT2 undocked must NOT be in guest list");
    assert!(!cids_1.contains(&offline_cid), "offline docked character must NOT be in guest list");

    // Negative: undocked pilot calls station.GetGuests
    let err_undocked = rpc(&ctx, &mut s2, "station", "GetGuests", &[])
        .await
        .expect_err("Undocked pilot calling station.GetGuests must fail");
    assert_eq!(err_undocked.code, "NotDockedInStation");

    // Dock PILOT2 at STATION_JITA via stationSvc.Dock
    ctx.advance_secs(12);
    rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(STATION_JITA)])
        .await
        .expect("stationSvc.Dock PILOT2");

    let guests_both = rpc(&ctx, &mut s1, "station", "GetGuests", &[])
        .await
        .expect("station.GetGuests both");
    let PyRep::List(guest_list_2) = guests_both else {
        panic!("station.GetGuests must return a List");
    };
    let mut cids_2 = Vec::new();
    for g in &guest_list_2 {
        let PyRep::Tuple(t) = g else {
            panic!("Guest entry must be a Tuple");
        };
        cids_2.push(t[0].as_i64().expect("characterID"));
    }
    assert!(cids_2.contains(&PILOT1), "PILOT1 must be in guest list");
    assert!(cids_2.contains(&PILOT2), "PILOT2 docked via stationSvc.Dock must be in guest list");
    assert!(!cids_2.contains(&offline_cid), "offline docked character must NOT be in guest list");

    // 2. stationSvc.GetStationItemBits
    let bits_rep = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationItemBits",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("stationSvc.GetStationItemBits");
    let PyRep::Tuple(bits) = bits_rep else {
        panic!("GetStationItemBits must return a 4-tuple");
    };
    assert_eq!(bits.len(), 4);
    assert_eq!(bits[0], PyRep::Integer(CORP_CALDARI_NAVY));
    assert_eq!(bits[1], PyRep::Integer(STATION_JITA));
    assert_eq!(bits[2], PyRep::Integer(14));
    assert_eq!(bits[3], PyRep::Integer(52678));

    let err_bits = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationItemBits",
        &[PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("GetStationItemBits with invalid station must fail");
    assert_eq!(err_bits.code, "StationNotFound");

    // 3. stationSvc.GetSolarSystem
    let sys_for_st = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("stationSvc.GetSolarSystem by station ID");
    assert_eq!(sys_for_st, PyRep::Integer(SYS_JITA as i64));

    let sys_info = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(SYS_JITA as i64)],
    )
    .await
    .expect("stationSvc.GetSolarSystem by solarSystemID");
    let PyRep::KeyVal(sys_kv) = sys_info else {
        panic!("GetSolarSystem by sysID must return KeyVal");
    };
    assert_eq!(sys_kv.get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(sys_kv.get("solarSystemName"), Some(&PyRep::String("Jita".into())));

    let err_sys = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("GetSolarSystem with unknown station ID must fail");
    assert_eq!(err_sys.code, "StationNotFound");

    // 4. stationSvc.GetStationsForOwner
    let owner_stations = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationsForOwner",
        &[PyRep::Integer(CORP_CALDARI_NAVY)],
    )
    .await
    .expect("stationSvc.GetStationsForOwner");
    let PyRep::List(st_list) = owner_stations else {
        panic!("GetStationsForOwner must return List");
    };
    assert!(!st_list.is_empty(), "Caldari Navy must own stations in SDE");
    let PyRep::KeyVal(first_st) = &st_list[0] else {
        panic!("Station record must be KeyVal");
    };
    assert_eq!(
        first_st.get("corporationID"),
        Some(&PyRep::Integer(CORP_CALDARI_NAVY))
    );
    assert!(first_st.get("stationID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let empty_st = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationsForOwner",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .expect("GetStationsForOwner for empty owner");
    assert_eq!(empty_st, PyRep::List(vec![]));

    // 5. stationSvc.GetAllianceSystems & stationSvc.GetSystemsForAlliance
    let test_alliance_id = 99_000_123i64;
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.system_states.entry(SYS_JITA).or_default();
        entry.solar_system_id = SYS_JITA;
        entry.sov_alliance_id = Some(test_alliance_id);
    });

    let ally_sys_1 = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetAllianceSystems",
        &[PyRep::Integer(test_alliance_id)],
    )
    .await
    .expect("stationSvc.GetAllianceSystems");
    assert_eq!(ally_sys_1, PyRep::List(vec![PyRep::Integer(SYS_JITA as i64)]));

    let ally_sys_2 = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSystemsForAlliance",
        &[PyRep::Integer(test_alliance_id)],
    )
    .await
    .expect("stationSvc.GetSystemsForAlliance");
    assert_eq!(ally_sys_2, PyRep::List(vec![PyRep::Integer(SYS_JITA as i64)]));

    let unheld_sys = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetAllianceSystems",
        &[PyRep::Integer(99_999_888)],
    )
    .await
    .expect("GetAllianceSystems for unheld alliance");
    assert_eq!(unheld_sys, PyRep::List(vec![]));

    // 6. corpStationMgr.DoStandingCheckForStationService
    let standing_ok = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("DoStandingCheckForStationService default standings");
    assert_eq!(standing_ok, PyRep::Bool(true));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.standings.insert(CORP_CALDARI_NAVY, -5.0);
        }
    });
    let err_standing = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with low standing must fail");
    assert_eq!(err_standing.code, "StandingTooLowForStationService");

    let err_unavailable = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(999_999), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with unoffered service must fail");
    assert_eq!(err_unavailable.code, "StationServiceUnavailable");

    let err_nostation = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with non-existent station must fail");
    assert_eq!(err_nostation.code, "StationNotFound");
}
