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
    // parity: feat:structures.npc-station-services, feat:structures.station-standing-check, rpc:corpStationMgr.DoStandingCheckForStationService, rpc:station.GetGuests, rpc:stationSvc.GetAllianceSystems, rpc:stationSvc.GetSolarSystem, rpc:stationSvc.GetStationItemBits, rpc:stationSvc.GetStationsForOwner, rpc:stationSvc.GetSystemsForAlliance
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

    // 7. stationSvc.GetStation & feat:structures.npc-station-services: services depend on station/operation
    let station_rep = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStation",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("stationSvc.GetStation");
    let PyRep::KeyVal(st_kv) = station_rep else {
        panic!("GetStation must return KeyVal");
    };
    assert_eq!(st_kv.get("stationID"), Some(&PyRep::Integer(STATION_JITA)));
    assert_eq!(st_kv.get("operationID"), Some(&PyRep::Integer(14)));
    let service_mask = st_kv.get("serviceMask").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(service_mask > 0, "serviceMask must be positive");
    let PyRep::List(svc_list) = st_kv.get("services").expect("services list") else {
        panic!("services must be List");
    };
    assert!(!svc_list.is_empty(), "Jita station must offer services");
    let svc_ids: Vec<i64> = svc_list.iter().filter_map(|s| s.as_i64()).collect();
    for req_sid in [3, 5, 7, 10, 13, 14] {
        assert!(svc_ids.contains(&req_sid), "Jita station must offer service {req_sid}");
    }

    let err_st = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStation",
        &[PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("GetStation with non-existent station must fail");
    assert_eq!(err_st.code, "StationNotFound");

    // 8. R-NSS-01 / R-NSS-02: verify station services depend on station/operation at real SDE stations
    // Station A: Jita IV-4 (STATION_JITA, operation 14: offers repair [13], reprocess [5], insurance [21], fitting [17], cloning [10], etc.)
    // Station B: Caldari Provisions Plantation (60000859, operation 1: no repair [13], no insurance [21], no cloning [10])
    // Station C: Turnur I Damaged Bureau (60004723, operation 120: only docking [22] and market [7], no reprocess [5], no LP store [25], no fitting [17])

    // At STATION_JITA (offers repair, insurance, reprocessing, fitting):
    s1.station_id = Some(STATION_JITA);
    let quotes_jita = rpc(&ctx, &mut s1, "repairSvc", "GetRepairQuotes", &[])
        .await
        .expect("GetRepairQuotes must succeed at Jita which offers repair");
    let PyRep::Dict(q_map) = quotes_jita else {
        panic!("GetRepairQuotes must return a Dict, got {quotes_jita:?}");
    };
    assert!(!q_map.is_empty(), "Repair quotes map should contain entries for ship");

    let ins_jita = rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrices", &[])
        .await
        .expect("GetInsurancePrices must succeed at Jita which offers insurance");
    let PyRep::List(ins_list) = ins_jita else {
        panic!("GetInsurancePrices must return a List, got {ins_jita:?}");
    };
    assert!(!ins_list.is_empty(), "Insurance prices list must not be empty");
    let PyRep::KeyVal(first_ins) = &ins_list[0] else {
        panic!("Expected KeyVal in insurance list");
    };
    assert!(first_ins.get("referenceValue").is_some(), "Expected referenceValue in insurance quote");

    let reproc_jita = rpc(&ctx, &mut s1, "reprocessingSvc", "GetQuotes", &[])
        .await
        .expect("GetQuotes must succeed at Jita which offers reprocessing");
    let PyRep::KeyVal(reproc_kv) = reproc_jita else {
        panic!("Expected KeyVal from GetQuotes, got {reproc_jita:?}");
    };
    assert!(reproc_kv.get("stationBaseYield").is_some(), "Expected stationBaseYield in reprocessing quotes");

    // Positive fitting test at Jita: fit module 100_000_003 into ship 100_000_001 slot 19 (MedSlot0)
    let fit_jita = rpc(
        &ctx,
        &mut s1,
        "invbroker",
        "Add",
        &[PyRep::Integer(100_000_003), PyRep::Integer(100_000_001), PyRep::Integer(19)],
    )
    .await;
    assert!(fit_jita.is_ok(), "Fitting module into ship slot must succeed at Jita which offers fitting");

    // Reset standing so standing check is clean:
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.standings.insert(CORP_CALDARI_NAVY, 0.0);
        }
    });

    // Standing check at Jita for repair (13), fitting (17), and bitmask 16 (reprocessing, service ID 5):
    let chk_jita_repair = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(STATION_JITA)]).await;
    assert!(chk_jita_repair.is_ok(), "DoStandingCheckForStationService for repair must succeed at Jita");
    let chk_jita_fit = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(17), PyRep::Integer(STATION_JITA)]).await;
    assert!(chk_jita_fit.is_ok(), "DoStandingCheckForStationService for fitting must succeed at Jita");
    let chk_jita_reproc_bm = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(16), PyRep::Integer(STATION_JITA)]).await;
    assert!(chk_jita_reproc_bm.is_ok(), "DoStandingCheckForStationService with bitmask 16 (reprocessing) must succeed at Jita");

    // GetStation serviceMask round-trip: service 5 (bit 4: 16) and service 17 (bit 16: 65536) must be present in mask:
    let st_rep = rpc(&ctx, &mut s1, "stationSvc", "GetStation", &[PyRep::Integer(STATION_JITA)]).await.expect("GetStation at Jita");
    let PyRep::KeyVal(st_kv) = st_rep else { panic!("Expected KeyVal from GetStation"); };
    let mask_jita = st_kv.get("serviceMask").and_then(|v| v.as_i64()).expect("serviceMask");
    assert!((mask_jita & (1i64 << (5 - 1))) != 0, "serviceMask must include bitmask 16 for reprocessing (service 5)");
    assert!((mask_jita & (1i64 << (17 - 1))) != 0, "serviceMask must include bit for fitting (service 17)");

    // Move to Station B (60000859: Plantation, does NOT offer repair, insurance, or clone bay):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(60000859);
        }
    });
    s1.station_id = Some(60000859);
    let err_repair_b = rpc(&ctx, &mut s1, "repairSvc", "GetRepairQuotes", &[])
        .await
        .expect_err("GetRepairQuotes must fail at station lacking repair service");
    assert_eq!(err_repair_b.code, "StationServiceUnavailable");

    let err_ins_b = rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrices", &[])
        .await
        .expect_err("GetInsurancePrices must fail at station lacking insurance service");
    assert_eq!(err_ins_b.code, "StationServiceUnavailable");

    let err_clone_b = rpc(&ctx, &mut s1, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect_err("InstallCloneInStation must fail at station lacking clone bay service");
    assert_eq!(err_clone_b.code, "StationHasNoCloneBay");

    let chk_b_repair = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(60000859)])
        .await
        .expect_err("DoStandingCheckForStationService for repair must fail at Plantation");
    assert_eq!(chk_b_repair.code, "StationServiceUnavailable");

    // Move to Station C (60004723: Damaged Bureau, does NOT offer reprocessing, LP store, or fitting):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(60004723);
        }
    });
    s1.station_id = Some(60004723);
    let err_reproc_c = rpc(&ctx, &mut s1, "reprocessingSvc", "GetQuotes", &[])
        .await
        .expect_err("GetQuotes must fail at station lacking reprocessing service");
    assert_eq!(err_reproc_c.code, "StationServiceUnavailable");

    let err_lp_c = rpc(&ctx, &mut s1, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .expect_err("GetAvailableOffersFromCorp must fail at station lacking LP store");
    assert_eq!(err_lp_c.code, "StationServiceUnavailable");

    // Negative fitting test at Station C: invbroker.Add module into ship slot must fail
    let err_fit_c = rpc(
        &ctx,
        &mut s1,
        "invbroker",
        "Add",
        &[PyRep::Integer(100_000_004), PyRep::Integer(100_000_001), PyRep::Integer(11)],
    )
    .await
    .expect_err("Fitting module at Station C lacking fitting service must fail");
    assert_eq!(err_fit_c.code, "StationServiceUnavailable");

    // Negative unfitting test at Station C: invbroker.Add fitted module out of ship slot must fail
    let err_unfit_c = rpc(
        &ctx,
        &mut s1,
        "invbroker",
        "Add",
        &[PyRep::Integer(100_000_003), PyRep::Integer(60004723), PyRep::Integer(4)],
    )
    .await
    .expect_err("Unfitting module at Station C lacking fitting service must fail");
    assert_eq!(err_unfit_c.code, "StationServiceUnavailable");

    // Negative standing check for fitting (17) at Station C:
    let chk_c_fit = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(17), PyRep::Integer(60004723)])
        .await
        .expect_err("DoStandingCheckForStationService for fitting must fail at Station C");
    assert_eq!(chk_c_fit.code, "StationServiceUnavailable");

    // Negative standing check for bitmask 16 (reprocessing) at Station C:
    let chk_c_reproc_bm = rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(16), PyRep::Integer(60004723)])
        .await
        .expect_err("DoStandingCheck with bitmask 16 must fail at Damaged Bureau");
    assert_eq!(chk_c_reproc_bm.code, "StationServiceUnavailable");

    let st_c_rep = rpc(&ctx, &mut s1, "stationSvc", "GetStation", &[PyRep::Integer(60004723)]).await.expect("GetStation at Station C");
    let PyRep::KeyVal(st_c_kv) = st_c_rep else { panic!("Expected KeyVal from GetStation C"); };
    let mask_c = st_c_kv.get("serviceMask").and_then(|v| v.as_i64()).expect("serviceMask");
    assert_eq!(mask_c & (1i64 << (5 - 1)), 0, "serviceMask at Damaged Bureau must NOT include bit for reprocessing");
    assert_eq!(mask_c & (1i64 << (17 - 1)), 0, "serviceMask at Damaged Bureau must NOT include bit for fitting");

    // Reset s1 station_id back to STATION_JITA
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(STATION_JITA);
        }
    });
    s1.station_id = Some(STATION_JITA);
}
