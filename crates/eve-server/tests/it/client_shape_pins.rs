//! Regression pin table for GH #1086 Section 3: Client empty-state & edge wire shapes.
//!
//! Every row pins the exact empty-state or edge `PyRep` value required by the V24.01 `code.ccp`
//! consumer (never merely `.is_ok()` or non-`None`), cites the client call site, links any
//! broader existing IT suite coverage, and includes negative assertions against forbidden shapes.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, StandingTransactionRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
use eve_testkit::client_harness::{extract_bound_id_from_wire_rep, ClientErrorResponse};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const DEMO_CHAR_ID: i64 = 90_000_001;
const DEMO_CORP_ID: i64 = 1_000_170;
const RIFTER_TYPE_ID: u32 = 587;
const TRITANIUM_TYPE_ID: u32 = 34;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    if let Some(sde) = SHARED_SDE.get() {
        Arc::clone(sde)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx.sde
    }
}

fn fresh_ctx() -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    common::pin_clock(&ctx);
    ctx
}

fn player_sess(
    sid: u64,
    char_id: Option<i64>,
    corp_id: Option<i64>,
    sys_id: Option<u32>,
    station_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(1);
    s.character_id = char_id;
    s.corporation_id = corp_id;
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

async fn call_wire(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, ClientErrorResponse> {
    common::borrow_wire_harness(ctx, sess)
        .call(service, bound_id, method, args, &HashMap::new())
        .await
}

#[tokio::test]
async fn pin_char_map_login_and_ui_empty_shapes() {
    // parity: rpc:charMgr.GetHomeStationRow
    // parity: rpc:loginCampaignManager.get_client_campaign_state
    // parity: rpc:seasonalLoginCampaignManager.get_active_campaign
    // parity: rpc:map.GetSecurityModifiedSystems
    // parity: rpc:populationCap.GetCharacterLoadSlot
    // parity: rpc:alert.BeanCount
    // parity: rpc:ship.SafeLogoff
    let ctx = fresh_ctx();
    let mut sess = player_sess(
        86_001,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );

    // 1. charMgr.GetHomeStationRow -> KeyVal with stationID / solarSystemID / stationTypeID.
    //    Client: eve/client/script/ui/shared/neocom/charactersheet.pyj:57 (_GetHomeStationInfo),
    //    eve/client/script/ui/shared/mapView/mapView.pyj:430,595, mapViewSolarSystem.pyj:646.
    //    Linked coverage: tests/it/rpc_char_lifecycle.rs:50-175,420-430.
    let home = call_wire(&ctx, &mut sess, "charMgr", None, "GetHomeStationRow", &[])
        .await
        .expect("charMgr.GetHomeStationRow");
    let PyRep::KeyVal(home_kv) = home else {
        panic!("charMgr.GetHomeStationRow must return KeyVal, got {home:?}");
    };
    assert_eq!(home_kv.get("stationID"), Some(&PyRep::Integer(JITA_STATION_ID)));
    assert_eq!(
        home_kv.get("solarSystemID"),
        Some(&PyRep::Integer(i64::from(JITA_SYSTEM_ID)))
    );
    assert!(
        home_kv
            .get("stationTypeID")
            .and_then(PyRep::as_i64)
            .is_some_and(|tid| tid > 0),
        "GetHomeStationRow must include positive integer stationTypeID: {home_kv:?}"
    );
    // Negative/edge: character with absent home_station_id falls back to school/starter station KeyVal, never None.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.home_station_id = 0;
        }
    });
    let fallback_home = call_wire(
        &ctx,
        &mut sess,
        "charMgr",
        None,
        "GetHomeStationRow",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("charMgr.GetHomeStationRow absent home_station_id fallback");
    let PyRep::KeyVal(fb_kv) = fallback_home else {
        panic!("absent home_station_id fallback must still return KeyVal, got {fallback_home:?}");
    };
    assert!(
        fb_kv.get("stationID").and_then(PyRep::as_i64).is_some_and(|id| id > 0)
            && fb_kv.get("solarSystemID").and_then(PyRep::as_i64).is_some_and(|id| id > 0)
            && fb_kv.get("stationTypeID").and_then(PyRep::as_i64).is_some_and(|id| id > 0),
        "fallback home station KeyVal missing required fields: {fb_kv:?}"
    );

    // 2. loginCampaignManager.get_client_campaign_state -> None by design when no character selected.
    //    Client: packages/loginrewards/client/loginCampaignService.pyj:127.
    //    Linked coverage: tests/it/login_campaign_and_milestone_claims.rs:53-68, tests/it/rpc_campaigns_and_telemetry.rs:83-89.
    let mut char_select_sess = player_sess(86_002, None, None, None, None);
    let camp_state = call_wire(
        &ctx,
        &mut char_select_sess,
        "loginCampaignManager",
        None,
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("loginCampaignManager.get_client_campaign_state");
    assert_eq!(
        camp_state,
        PyRep::None,
        "get_client_campaign_state with no selected character must return None"
    );

    // 3. seasonalLoginCampaignManager.get_active_campaign -> 4-tuple of None when inactive.
    //    Client: packages/loginrewards/client/seasonalLoginCampaignService.pyj:56 (UNPACK_SEQUENCE 4).
    //    Linked coverage: tests/it/store_fastcheckout_seasonal_live.rs:501-506.
    let seasonal = call_wire(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        None,
        "get_active_campaign",
        &[],
    )
    .await
    .expect("seasonalLoginCampaignManager.get_active_campaign");
    assert_eq!(
        seasonal,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]),
        "inactive seasonalLoginCampaignManager.get_active_campaign must unpack as 4-tuple of None"
    );

    // 4. map.GetSecurityModifiedSystems -> util.KeyVal-compatible Rowset rows on wire.
    //    Client: eve/client/script/ui/login/charSelection/characterSlots.pyj:1399
    //    (_GetSecurityModifierText calls .Index('solarSystemID') and reads .security / .modifiedSecurity).
    //    Linked coverage: tests/it/client_login_rpcs.rs:277-288, tests/it/gameplay_services_and_rpcs.rs:1600.
    let wire_mod = call_wire(
        &ctx,
        &mut sess,
        "map",
        None,
        "GetSecurityModifiedSystems",
        &[],
    )
    .await
    .expect("map.GetSecurityModifiedSystems wire");
    let PyRep::Rowset {
        columns: mod_cols,
        rows: mod_rows,
    } = wire_mod
    else {
        panic!("wire map.GetSecurityModifiedSystems must normalize to Rowset, got {wire_mod:?}");
    };
    assert_eq!(
        mod_cols,
        vec!["solarSystemID", "security", "modifiedSecurity"],
        "Rowset rows must expose solarSystemID/security/modifiedSecurity for client .Index('solarSystemID')"
    );
    for row in &mod_rows {
        assert_eq!(row.len(), 3, "each GetSecurityModifiedSystems row must have 3 columns");
        assert!(row[0].as_i64().is_some(), "solarSystemID must be integer");
        assert!(row[1].as_f64().is_some(), "security must be float");
        assert!(row[2].as_f64().is_some(), "modifiedSecurity must be float");
    }

    // 5. populationCap.GetCharacterLoadSlot -> {systemID: None} falsy sentinel (Note: #1086 prose
    //    referred to `{systemID: 0}` as an unthrottled falsy sentinel; EvoMu pins `{cur_sys: None}`).
    //    Client: eve/client/script/ui/login/charSelection/characterSelection.pyj:949 (TryEnterGame).
    //    Linked coverage: tests/it/character_account_features_tests.rs:140-160, tests/it/client_login_rpcs.rs:313-331,
    //    tests/it/space_travel_shipcaster_suite.rs:554,589.
    let load_slot = call_wire(
        &ctx,
        &mut char_select_sess,
        "populationCap",
        None,
        "GetCharacterLoadSlot",
        &[PyRep::Integer(DEMO_CHAR_ID)],
    )
    .await
    .expect("populationCap.GetCharacterLoadSlot");
    assert_eq!(
        load_slot,
        PyRep::Dict(vec![(
            PyRep::Integer(i64::from(JITA_SYSTEM_ID)),
            PyRep::None
        )]),
        "uncapped GetCharacterLoadSlot must return single-entry {{cur_sys: None}} dict"
    );

    // 6. alert.BeanCount -> 2-tuple (None, None) for unknown stack trace hash.
    //    Client: carbon/common/script/sys/alert.pyj:670,744,863 (UNPACK_SEQUENCE 2).
    //    Linked coverage: tests/it/rpc_account_air_banned.rs:159-168, tests/it/client_login_rpcs.rs:50-70.
    let bean = call_wire(
        &ctx,
        &mut sess,
        "alert",
        None,
        "BeanCount",
        &[PyRep::Integer(0xDEAD_BEEF)],
    )
    .await
    .expect("alert.BeanCount unknown hash");
    assert_eq!(
        bean,
        PyRep::Tuple(vec![PyRep::None, PyRep::None]),
        "unknown alert.BeanCount hash must return 2-element (None, None)"
    );

    // 7. ship.SafeLogoff -> empty list [] when clean in space, non-empty list when docked.
    //    Client: eve/client/script/ui/services/menuSvcExtras/menuFunctions.pyj:250 (SafeLogoff).
    //    Linked coverage: tests/it/abyssal_deadspace.rs:247-248, tests/it/character_skill_features_t12.rs:246, tests/it/combat.rs:135-143.
    let docked_logoff = call_wire(&ctx, &mut sess, "ship", None, "SafeLogoff", &[])
        .await
        .expect("ship.SafeLogoff docked");
    let PyRep::List(docked_reasons) = docked_logoff else {
        panic!("docked ship.SafeLogoff must return List, got {docked_logoff:?}");
    };
    assert!(
        !docked_reasons.is_empty(),
        "docked ship.SafeLogoff must return non-empty blocking condition labels"
    );
    let mut pilot = common::gm_pilot_in_space(&ctx, 86_003, 90_086_003, RIFTER_TYPE_ID, JITA_SYSTEM_ID).await;
    let clean_logoff = call_wire(&ctx, &mut pilot.sess, "ship", None, "SafeLogoff", &[])
        .await
        .expect("ship.SafeLogoff clean in space");
    assert_eq!(
        clean_logoff,
        PyRep::List(vec![]),
        "clean in-space ship.SafeLogoff must return exact empty list []"
    );
    let aborted = call_wire(&ctx, &mut pilot.sess, "ship", None, "AbortSafeLogoff", &[])
        .await
        .expect("ship.AbortSafeLogoff");
    assert_eq!(aborted, PyRep::Bool(true));

    eve_testkit::attest(&[
        "rpc:charMgr.GetHomeStationRow",
        "rpc:loginCampaignManager.get_client_campaign_state",
        "rpc:seasonalLoginCampaignManager.get_active_campaign",
        "rpc:map.GetSecurityModifiedSystems",
        "rpc:populationCap.GetCharacterLoadSlot",
        "rpc:alert.BeanCount",
        "rpc:ship.SafeLogoff",
    ]);
}

#[tokio::test]
async fn pin_sov_fw_insurgency_and_alliance_system_empty_shapes() {
    // parity: rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem
    // parity: rpc:devIndexManager.GetDevelopmentIndicesForSystem
    // parity: rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStates
    // parity: rpc:stationSvc.GetAllianceSystems
    // parity: rpc:insurgencySolarsystem.GetLocalCampaignClientSnapshot
    let ctx = fresh_ctx();
    let mut sess = player_sess(
        86_010,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );

    // 1. sovMgr.GetSovStructuresInfoForLocalSolarSystem -> empty list [] in non-sov system.
    //    Client: eve/client/script/ui/services/sovSvc.pyj:104.
    //    Linked coverage: tests/it/rpc_sov_fw_warzone.rs:332-343.
    let sov_structs = call_wire(
        &ctx,
        &mut sess,
        "sovMgr",
        None,
        "GetSovStructuresInfoForLocalSolarSystem",
        &[],
    )
    .await
    .expect("sovMgr.GetSovStructuresInfoForLocalSolarSystem");
    assert_eq!(
        sov_structs,
        PyRep::List(vec![]),
        "non-sov system must return exact empty list [] from GetSovStructuresInfoForLocalSolarSystem"
    );

    // 2. devIndexManager.GetDevelopmentIndicesForSystem -> empty dict {} for unowned/unindexed system.
    //    Client: eve/client/script/ui/services/sovSvc.pyj:338,340 & panelSov.pyj:62,94 (.get(attrID, None)).
    let empty_dev_idx = call_wire(
        &ctx,
        &mut sess,
        "devIndexManager",
        None,
        "GetDevelopmentIndicesForSystem",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID))],
    )
    .await
    .expect("devIndexManager.GetDevelopmentIndicesForSystem empty");
    assert_eq!(
        empty_dev_idx,
        PyRep::Dict(vec![]),
        "unowned highsec system must return exact empty dict {{}}, never dummy indices or []"
    );
    // Negative/populated contrast: sov-claimed system with kills returns 3 index keys (1583, 1584, 1615).
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states
            .entry(JITA_SYSTEM_ID)
            .or_default()
            .sov_alliance_id = Some(99_000_001);
        snap.sov_runtime.sov_npc_kills.insert(JITA_SYSTEM_ID, 2_000);
    });
    let seeded_dev_idx = call_wire(
        &ctx,
        &mut sess,
        "devIndexManager",
        None,
        "GetDevelopmentIndicesForSystem",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID))],
    )
    .await
    .expect("devIndexManager.GetDevelopmentIndicesForSystem seeded");
    let PyRep::Dict(idx_entries) = seeded_dev_idx else {
        panic!("populated GetDevelopmentIndicesForSystem must return Dict");
    };
    assert_eq!(idx_entries.len(), 3, "sov system must expose 3 dev index entries");

    // 3. fwWarzoneSolarsystem.GetAllWarzonesOccupationStates -> dict (supports .itervalues()), never list/None.
    //    Client: eve/client/script/ui/services/fwWarzoneSvc.pyj:105 (.itervalues()), mapViewData.pyj:109, starMapSvc.pyj:1929.
    //    Linked coverage: tests/it/rpc_sov_fw_warzone.rs:479-491, tests/it/fw_parity_complete.rs:60-120.
    let wz_states = call_wire(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        None,
        "GetAllWarzonesOccupationStates",
        &[],
    )
    .await
    .expect("fwWarzoneSolarsystem.GetAllWarzonesOccupationStates");
    let PyRep::Dict(wz_entries) = wz_states else {
        panic!("GetAllWarzonesOccupationStates must return Dict for .itervalues(), got {wz_states:?}");
    };
    for (wz_id, wz_map) in &wz_entries {
        assert!(wz_id.as_i64().is_some(), "warzone key must be integer");
        assert!(
            matches!(wz_map, PyRep::Dict(_)),
            "each .itervalues() warzone element must be a Dict, got {wz_map:?}"
        );
    }
    // Even when FW warzone occupations are cleared (or re-backfilled), outer container and values remain Dicts (never [] or None).
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        snap.fw_runtime.system_victory_points.clear();
        snap.system_states.values_mut().for_each(|st| st.fw_contested_pct = 0.0);
    });
    let empty_wz = call_wire(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        None,
        "GetAllWarzonesOccupationStates",
        &[],
    )
    .await
    .expect("fwWarzoneSolarsystem.GetAllWarzonesOccupationStates cleared");
    assert!(
        matches!(empty_wz, PyRep::Dict(_)),
        "empty GetAllWarzonesOccupationStates must remain Dict, got {empty_wz:?}"
    );

    // 4. stationSvc.GetAllianceSystems -> iterable Rowset(solarSystemID, allianceID), never List<Integer>.
    //    Client: eve/client/script/ui/shared/mapView/mapViewData.pyj:111 & starMapSvc.pyj:1932
    //    (`for x in allianceSystemCache: allianceSolarSystems[x.solarSystemID] = x.allianceID`).
    let pop_ally = call_wire(&ctx, &mut sess, "stationSvc", None, "GetAllianceSystems", &[])
        .await
        .expect("stationSvc.GetAllianceSystems populated");
    let PyRep::Rowset {
        columns: ally_cols,
        rows: ally_rows,
    } = pop_ally
    else {
        panic!("stationSvc.GetAllianceSystems must return Rowset(solarSystemID, allianceID)");
    };
    assert_eq!(ally_cols, vec!["solarSystemID", "allianceID"]);
    assert!(
        ally_rows.contains(&vec![
            PyRep::Integer(i64::from(JITA_SYSTEM_ID)),
            PyRep::Integer(99_000_001)
        ]),
        "seeded alliance system row missing from GetAllianceSystems: {ally_rows:?}"
    );
    // Empty state: clear sov alliances & structures -> still Rowset(solarSystemID, allianceID) with 0 rows.
    ctx.db.with_mut_snapshot(|snap| {
        for st in snap.system_states.values_mut() {
            st.sov_alliance_id = None;
        }
        snap.structures.clear();
    });
    let empty_ally = call_wire(&ctx, &mut sess, "stationSvc", None, "GetAllianceSystems", &[])
        .await
        .expect("stationSvc.GetAllianceSystems empty");
    assert_eq!(
        empty_ally,
        PyRep::rowset(vec!["solarSystemID", "allianceID"], vec![]),
        "empty GetAllianceSystems must return Rowset(solarSystemID, allianceID) with empty rows"
    );

    // 5. insurgencySolarsystem.GetLocalCampaignClientSnapshot -> (systemID, None) 2-tuple, never [].
    //    Client: eve/client/script/ui/services/insurgencyCampaignSvc.pyj:68,71 (UNPACK_SEQUENCE 2).
    //    Linked coverage: tests/it/rpc_fw_insurgency_crimewatch.rs:30-95.
    let ins_snap = call_wire(
        &ctx,
        &mut sess,
        "insurgencySolarsystem",
        None,
        "GetLocalCampaignClientSnapshot",
        &[],
    )
    .await
    .expect("insurgencySolarsystem.GetLocalCampaignClientSnapshot");
    assert_eq!(
        ins_snap,
        PyRep::Tuple(vec![PyRep::Integer(i64::from(JITA_SYSTEM_ID)), PyRep::None]),
        "inactive insurgency GetLocalCampaignClientSnapshot must return (systemID, None), never []"
    );

    eve_testkit::attest(&[
        "rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem",
        "rpc:devIndexManager.GetDevelopmentIndicesForSystem",
        "rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStates",
        "rpc:stationSvc.GetAllianceSystems",
        "rpc:insurgencySolarsystem.GetLocalCampaignClientSnapshot",
    ]);
}

#[tokio::test]
async fn pin_config_multi_owners_and_standing_mgr_sanitation() {
    // parity: rpc:config.GetMultiOwnersEx
    // parity: rpc:standingMgr.GetCharStandings
    // parity: rpc:standingMgr.GetCorpStandings
    // parity: rpc:standingMgr.GetNPCNPCStandings
    // parity: rpc:standingMgr.GetStandingTransactions
    // parity: rpc:standingMgr.GetStandingCompositions
    let ctx = fresh_ctx();
    let mut sess = player_sess(
        86_020,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );

    // 1. config.GetMultiOwnersEx -> true owner-range fallbacks resolve to their V24 owner
    //    typeIDs (2 corp, 30 faction, 16159 alliance, 1373 character), while inventory items
    //    and non-owner `preInv64Items` IDs (100M..2.1B) are omitted so V24 `idCheckers.IsCharacter`
    //    catches `KeyError` -> `False` and never routes `ShowInfo` into `charMgr.GetPublicInfo3`.
    //    Client: eve/common/script/sys/eveCfg.pyj (LoadEveOwners / EveOwners),
    //    eve/common/script/sys/idCheckers.pyj (IsEvePlayerCharacter).
    let unknown_npc_corp = 1_999_123_i64;
    let unknown_faction = 599_123_i64;
    let unknown_alliance = 99_999_123_i64;
    let unknown_gen2_char = 90_999_123_i64;
    let cargo_item_id = 1_896_953_512_i64;
    let unknown_preinv64_id = 888_777_666_i64;
    let valid_faction = 500_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_item_id,
            InventoryItemRecord {
                item_id: cargo_item_id,
                type_id: 222,
                owner_id: DEMO_CHAR_ID,
                location_id: 1_896_964_019,
                flag: InventoryFlag::Cargo,
                quantity: 1100,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let owners_rep = call_wire(
        &ctx,
        &mut sess,
        "config",
        None,
        "GetMultiOwnersEx",
        &[PyRep::List(vec![
            PyRep::Integer(valid_faction),
            PyRep::Integer(unknown_npc_corp),
            PyRep::Integer(unknown_faction),
            PyRep::Integer(unknown_alliance),
            PyRep::Integer(unknown_gen2_char),
            PyRep::Integer(cargo_item_id),
            PyRep::Integer(unknown_preinv64_id),
            PyRep::Integer(JITA_STATION_ID),
        ])],
    )
    .await
    .expect("config.GetMultiOwnersEx mixed");
    let PyRep::TupleSet {
        columns,
        rows: owner_rows,
    } = owners_rep
    else {
        panic!("config.GetMultiOwnersEx must return TupleSet, got {owners_rep:?}");
    };
    assert_eq!(
        columns,
        vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]
    );
    assert_eq!(
        owner_rows.len(),
        5,
        "GetMultiOwnersEx must resolve only true owner ranges and omit items/non-owners: {owner_rows:?}"
    );
    assert_eq!(owner_rows[0].first(), Some(&PyRep::Integer(valid_faction)));
    assert_eq!(owner_rows[0].get(2), Some(&PyRep::Integer(30)));
    assert_eq!(owner_rows[0].get(3), Some(&PyRep::Bool(false)));
    assert_eq!(owner_rows[1].first(), Some(&PyRep::Integer(unknown_npc_corp)));
    assert_eq!(owner_rows[1].get(2), Some(&PyRep::Integer(2)));
    assert_eq!(owner_rows[1].get(3), Some(&PyRep::Bool(false)));
    assert_eq!(owner_rows[2].first(), Some(&PyRep::Integer(unknown_faction)));
    assert_eq!(owner_rows[2].get(2), Some(&PyRep::Integer(30)));
    assert_eq!(owner_rows[2].get(3), Some(&PyRep::Bool(false)));
    assert_eq!(owner_rows[3].first(), Some(&PyRep::Integer(unknown_alliance)));
    assert_eq!(owner_rows[3].get(2), Some(&PyRep::Integer(16159)));
    assert_eq!(owner_rows[3].get(3), Some(&PyRep::Bool(false)));
    assert_eq!(owner_rows[4].first(), Some(&PyRep::Integer(unknown_gen2_char)));
    assert_eq!(owner_rows[4].get(2), Some(&PyRep::Integer(1373)));
    assert_eq!(owner_rows[4].get(3), Some(&PyRep::None));
    assert_eq!(owner_rows[4].get(4), Some(&PyRep::None));
    assert!(
        owner_rows[4]
            .get(1)
            .and_then(PyRep::as_str)
            .is_some_and(|s| !s.is_empty()),
        "character fallback owner row must carry a non-empty ownerName"
    );
    assert_eq!(
        call_wire(
            &ctx,
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![PyRep::Integer(cargo_item_id)])],
        )
        .await
        .expect("cargo item GetMultiOwnersEx"),
        PyRep::tuple_set(
            vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"],
            vec![],
        ),
        "cargo inventory item must be omitted from GetMultiOwnersEx so ShowInfo never treats it as a Character"
    );
    // Empty input & invalid non-integer negative checks.
    assert_eq!(
        call_wire(
            &ctx,
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![])],
        )
        .await
        .expect("empty GetMultiOwnersEx"),
        PyRep::tuple_set(
            vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"],
            vec![],
        )
    );
    let bad_owner_arg = call_wire(
        &ctx,
        &mut sess,
        "config",
        None,
        "GetMultiOwnersEx",
        &[PyRep::List(vec![PyRep::String("not-an-id".into())])],
    )
    .await;
    assert!(
        bad_owner_arg.is_err(),
        "non-integer ID in GetMultiOwnersEx must fail cleanly, got {bad_owner_arg:?}"
    );

    // 2. standingMgr rowsets never contain invalid owner IDs.
    //    Client: eve/client/script/ui/services/standingsvc.pyj:118-148,261-290
    //    (calls idCheckers.IsCorporation / cfg.eveowners.Prime on fromID/toID).
    let invalid_ids = [0_i64, -5, i64::from(JITA_SYSTEM_ID), JITA_STATION_ID, 999_999];
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&DEMO_CHAR_ID).expect("demo char");
        ch.standings.insert(500_001, 2.5);
        ch.standings.insert(1_000_035, 1.25);
        for &bad in &invalid_ids {
            ch.standings.insert(bad, 4.0);
        }
        let tx_list = snap.standing_transactions.entry(DEMO_CHAR_ID).or_default();
        tx_list.push(StandingTransactionRecord {
            event_id: 90_001,
            from_id: 1_000_035,
            to_id: DEMO_CHAR_ID,
            modification: 0.25,
            event_type_id: 24,
        });
        for (idx, &bad) in invalid_ids.iter().enumerate() {
            tx_list.push(StandingTransactionRecord {
                event_id: 90_010 + idx as i64,
                from_id: bad,
                to_id: DEMO_CHAR_ID,
                modification: 0.1,
                event_type_id: 24,
            });
            tx_list.push(StandingTransactionRecord {
                event_id: 90_020 + idx as i64,
                from_id: 1_000_035,
                to_id: bad,
                modification: 0.1,
                event_type_id: 24,
            });
        }
    });

    let char_st = call_wire(&ctx, &mut sess, "standingMgr", None, "GetCharStandings", &[])
        .await
        .expect("standingMgr.GetCharStandings");
    let PyRep::Rowset { columns: cs_cols, rows: cs_rows } = char_st else {
        panic!("GetCharStandings must return Rowset");
    };
    assert_eq!(cs_cols, vec!["fromID", "standing"]);
    let cs_ids: Vec<i64> = cs_rows.iter().filter_map(|r| r[0].as_i64()).collect();
    assert!(cs_ids.contains(&500_001) && cs_ids.contains(&1_000_035));
    for bad in &invalid_ids {
        assert!(!cs_ids.contains(bad), "GetCharStandings leaked invalid ownerID {bad}");
    }

    let corp_st = call_wire(&ctx, &mut sess, "standingMgr", None, "GetCorpStandings", &[])
        .await
        .expect("standingMgr.GetCorpStandings");
    let PyRep::Rowset { rows: corp_rows, .. } = corp_st else {
        panic!("GetCorpStandings must return Rowset");
    };
    for r in &corp_rows {
        let fid = r[0].as_i64().unwrap_or(0);
        assert!(!invalid_ids.contains(&fid) && fid > 0, "GetCorpStandings leaked invalid {fid}");
    }

    let npc_st = call_wire(&ctx, &mut sess, "standingMgr", None, "GetNPCNPCStandings", &[])
        .await
        .expect("standingMgr.GetNPCNPCStandings");
    let PyRep::Rowset { rows: npc_rows, .. } = npc_st else {
        panic!("GetNPCNPCStandings must return Rowset");
    };
    for r in &npc_rows {
        let from_id = r[0].as_i64().unwrap_or(0);
        let to_id = r[1].as_i64().unwrap_or(0);
        assert!(from_id > 0 && to_id > 0, "GetNPCNPCStandings leaked non-positive ID");
    }

    let tx_rep = call_wire(
        &ctx,
        &mut sess,
        "standingMgr",
        None,
        "GetStandingTransactions",
        &[],
    )
    .await
    .expect("standingMgr.GetStandingTransactions");
    let PyRep::Rowset { rows: tx_rows, .. } = tx_rep else {
        panic!("GetStandingTransactions must return Rowset");
    };
    assert!(
        tx_rows.iter().any(|r| r[1].as_i64() == Some(1_000_035) && r[2].as_i64() == Some(DEMO_CHAR_ID)),
        "valid standing transaction missing"
    );
    for r in &tx_rows {
        let fid = r[1].as_i64().unwrap_or(0);
        let tid = r[2].as_i64().unwrap_or(0);
        assert!(
            !invalid_ids.contains(&fid) && !invalid_ids.contains(&tid) && fid > 0 && tid > 0,
            "GetStandingTransactions leaked invalid owner ID ({fid}, {tid})"
        );
    }

    // Negative filter check: direct query for an invalid owner ID returns empty list/rowset.
    let bad_comp = call_wire(
        &ctx,
        &mut sess,
        "standingMgr",
        None,
        "GetStandingCompositions",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID))],
    )
    .await
    .expect("standingMgr.GetStandingCompositions invalid from_id");
    assert_eq!(bad_comp, PyRep::List(vec![]));

    let all_comp = call_wire(
        &ctx,
        &mut sess,
        "standingMgr",
        None,
        "GetStandingCompositions",
        &[],
    )
    .await
    .expect("standingMgr.GetStandingCompositions");
    let PyRep::List(comp_items) = all_comp else {
        panic!("GetStandingCompositions must return List");
    };
    assert!(
        !comp_items.is_empty(),
        "GetStandingCompositions must retain valid seeded standing compositions"
    );
    for entry in &comp_items {
        let from_id = entry.dict_get("fromID").and_then(PyRep::as_i64).unwrap_or(0);
        let to_id = entry.dict_get("toID").and_then(PyRep::as_i64).unwrap_or(0);
        assert!(
            !invalid_ids.contains(&from_id) && from_id > 0,
            "GetStandingCompositions leaked invalid fromID {from_id}"
        );
        assert!(
            !invalid_ids.contains(&to_id) && to_id > 0,
            "GetStandingCompositions leaked invalid toID {to_id}"
        );
    }

    eve_testkit::attest(&[
        "rpc:config.GetMultiOwnersEx",
        "rpc:standingMgr.GetCharStandings",
        "rpc:standingMgr.GetCorpStandings",
        "rpc:standingMgr.GetNPCNPCStandings",
        "rpc:standingMgr.GetStandingTransactions",
        "rpc:standingMgr.GetStandingCompositions",
    ]);
}

#[tokio::test]
async fn pin_on_item_change_uses_ix_stack_size_never_ix_quantity() {
    // parity: notify:OnItemChange
    // Client: eve/client/script/environment/invCache.pyj:250 (ProcessOnItemChange rejects ixQuantity=5
    // on stack deltas and reads ixStackSize=9).
    // Linked coverage: tests/it/regressions_v24_audit.rs:1627-1686.
    let ctx = fresh_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.corporation_id = DEMO_CORP_ID;
        }
    });
    let kw = HashMap::new();
    let mut harness = common::new_wire_harness(&ctx, 86_030);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(DEMO_CHAR_ID)],
            &kw,
        )
        .await;

    let stack_id = ctx.db.with_mut_snapshot(|snap| {
        let id = snap.next_entity_id + 10;
        snap.next_entity_id = id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        id
    });

    let bind_rep = harness
        .call_ok(
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(JITA_STATION_ID), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
        )
        .await;
    let mgr_bid = extract_bound_id_from_wire_rep(&bind_rep).expect("mgr bound id");
    let hangar_inv = harness
        .call_ok(
            "invbroker",
            Some(mgr_bid),
            "GetInventory",
            &[PyRep::Integer(10_004), PyRep::None],
            &kw,
        )
        .await;
    let hangar_bid = extract_bound_id_from_wire_rep(&hangar_inv).expect("hangar bound id");

    let _ = harness.drain_notifications();
    let _ = harness.drain_frames();
    let _new_stack_id = harness
        .call_ok(
            "invbroker",
            Some(hangar_bid),
            "SplitStack",
            &[PyRep::Integer(stack_id), PyRep::Integer(3)],
            &kw,
        )
        .await;

    let notifs = harness.drain_notifications();
    let item_changes: Vec<_> = notifs
        .iter()
        .filter(|(event, _)| event == "OnItemChange")
        .collect();
    assert!(
        !item_changes.is_empty(),
        "SplitStack must emit OnItemChange notifications"
    );
    let mut saw_ix_stack_size = false;
    for (_, args) in &item_changes {
        let Some(PyRep::Dict(change_dict)) = args.get(1) else {
            panic!("OnItemChange args[1] must be a change Dict, got {args:?}");
        };
        assert!(
            !change_dict.iter().any(|(k, _)| k.as_i64() == Some(5)),
            "OnItemChange stack delta must NEVER include ixQuantity (5): {change_dict:?}"
        );
        if change_dict.iter().any(|(k, _)| k.as_i64() == Some(9)) {
            saw_ix_stack_size = true;
        }
    }
    assert!(
        saw_ix_stack_size,
        "SplitStack OnItemChange must report stack delta under ixStackSize (9)"
    );

    eve_testkit::attest(&["notify:OnItemChange"]);
}
