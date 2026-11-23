//! Regression pin table for GH #1086 Section 3: Client empty-state & edge wire shapes.
//!
//! Every row pins the exact empty-state or edge `PyRep` value required by the V24.01 `code.ccp`
//! consumer (never merely `.is_ok()` or non-`None`), cites the client call site, links any
//! broader existing IT suite coverage, and includes negative assertions against forbidden shapes.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, JumpCloneRecord, StandingTransactionRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::client_wire::ClientWireSession;
use eve_server::router::ServerContext;
use eve_testkit::client_contract::{
    validate_reply_shape_with_metrics, ClientContractRatchet, SemanticLookupContext,
};
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

async fn wire_session_call_ok(
    wire: &mut ClientWireSession<'_>,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> PyRep {
    let out = wire.call_wire(service, bound_id, method, args, kwargs).await;
    assert!(
        out.marshal_errors.is_empty(),
        "{service}.{method} produced wire marshal errors: {:?}",
        out.marshal_errors
    );
    out.rpc_result
        .unwrap_or_else(|e| panic!("{service}.{method} failed on ClientWireSession: {e:?}"))
}

fn assert_contract_enforcing(ctx: &ServerContext, rpc_name: &str, rep: &PyRep) {
    let raw = std::fs::read_to_string(ClientContractRatchet::default_expectations_path())
        .expect("read client_expectations.json");
    let root: serde_json::Value =
        serde_json::from_str(&raw).expect("parse client_expectations.json");
    let shape = root["rpc"][rpc_name]["reply_shape"]
        .as_object()
        .unwrap_or_else(|| panic!("missing reply_shape for {rpc_name}"));
    let sem = SemanticLookupContext {
        sde: &ctx.sde,
        db: &ctx.db,
    };
    let outcome = validate_reply_shape_with_metrics(Some(&sem), rpc_name, rep, shape);
    assert!(
        outcome.violations.is_empty(),
        "{rpc_name} contract violations: {:#?}\nreply={rep:?}",
        outcome.violations
    );
    assert!(
        !outcome.had_unexercised_collection_elements,
        "{rpc_name} contract check was vacuous (unexercised collection elements); reply={rep:?}"
    );
    assert!(
        outcome.paths_checked > 0,
        "{rpc_name} contract check checked 0 paths"
    );
}

#[tokio::test]
async fn pin_1187_raffle_history_wire_shape() {
    // parity: rpc:raffleProxy.GetMyRaffleHistory
    // Client: packages/raffles/client/raffle_service.pyj:130,136 (get_history).
    let ctx = fresh_ctx();
    let mut ship_types: Vec<u32> = ctx
        .sde
        .types
        .iter()
        .filter(|(&tid, t)| t.published && t.group_id == 25 && tid != 52_568)
        .map(|(&tid, _)| tid)
        .collect();
    ship_types.sort_unstable();
    let (type_a, type_b) = (
        ship_types.first().copied().unwrap_or(RIFTER_TYPE_ID),
        ship_types.get(1).copied().unwrap_or(603),
    );
    let (item_a, item_b) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
        snap.next_entity_id += 1;
        let id_a = snap.next_entity_id;
        snap.items.insert(
            id_a,
            InventoryItemRecord {
                item_id: id_a,
                type_id: type_a,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let id_b = snap.next_entity_id;
        snap.items.insert(
            id_b,
            InventoryItemRecord {
                item_id: id_b,
                type_id: type_b,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let cores_id = snap.next_entity_id;
        snap.items.insert(
            cores_id,
            InventoryItemRecord {
                item_id: cores_id,
                type_id: 52_568,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        (id_a, id_b)
    });

    let sess = player_sess(
        87_101,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );
    let mut wire = ClientWireSession::from_session(&ctx, sess);
    let kw = HashMap::new();

    let created_a = wire_session_call_ok(
        &mut wire,
        "raffleProxy",
        None,
        "CreateRaffle",
        &[
            PyRep::Integer(item_a),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
        &kw,
    )
    .await;
    let raffle_id_a = created_a
        .as_i64()
        .expect("created raffle A id");

    let created_b = wire_session_call_ok(
        &mut wire,
        "raffleProxy",
        None,
        "CreateRaffle",
        &[
            PyRep::Integer(item_b),
            PyRep::Integer(8),
            PyRep::Real(200_000.0),
        ],
        &kw,
    )
    .await;
    let raffle_id_b = created_b
        .as_i64()
        .expect("created raffle B id");

    let hist_rep = wire_session_call_ok(
        &mut wire,
        "raffleProxy",
        None,
        "GetMyRaffleHistory",
        &[],
        &kw,
    )
    .await;
    let PyRep::Tuple(parts) = &hist_rep else {
        panic!("GetMyRaffleHistory must return 2-tuple on wire, got {hist_rep:?}");
    };
    assert_eq!(parts.len(), 2, "GetMyRaffleHistory must return 2-tuple");
    let PyRep::List(rows) = &parts[0] else {
        panic!("GetMyRaffleHistory[0] must be a List, got {:?}", parts[0]);
    };
    assert_eq!(rows.len(), 2, "Expected both created raffles in history: {rows:?}");
    assert!(
        parts[1].as_i64().is_some_and(|n| n > 0),
        "GetMyRaffleHistory[1] must be positive page size, got {:?}",
        parts[1]
    );

    let got_pairs: Vec<(i64, u32, i64, i64)> = rows
        .iter()
        .map(|r| {
            (
                r.dict_get("raffle_id").and_then(PyRep::as_i64).expect("raffle_id"),
                r.dict_get("type_id").and_then(PyRep::as_i64).expect("type_id") as u32,
                r.dict_get("owner_id").and_then(PyRep::as_i64).expect("owner_id"),
                r.dict_get("running_id").and_then(PyRep::as_i64).expect("running_id"),
            )
        })
        .collect();
    assert_eq!(
        got_pairs,
        vec![
            (raffle_id_b, type_b, DEMO_CHAR_ID, raffle_id_b),
            (raffle_id_a, type_a, DEMO_CHAR_ID, raffle_id_a),
        ],
        "GetMyRaffleHistory must return real created raffles (not stub type_id=0)"
    );
    assert_contract_enforcing(&ctx, "raffleProxy.GetMyRaffleHistory", &hist_rep);
    eve_testkit::attest(&["rpc:raffleProxy.GetMyRaffleHistory"]);
}

#[tokio::test]
async fn pin_1187_trash_items_wire_shape() {
    // parity: rpc:invbroker.TrashItems
    // Client: eve/client/script/ui/services/menuSvcExtras/invItemFunctions.pyj:641 (TrashInvItems).
    let ctx = fresh_ctx();
    let item_id = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                ..Default::default()
            },
        );
        id
    });

    let sess = player_sess(
        87_102,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );
    let mut wire = ClientWireSession::from_session(&ctx, sess);
    let kw = HashMap::new();

    let bind_rep = wire_session_call_ok(
        &mut wire,
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
    let mgr_bid = extract_bound_id_from_wire_rep(&bind_rep).expect("invbroker bound id");

    let trash_rep = wire_session_call_ok(
        &mut wire,
        "invbroker",
        Some(mgr_bid),
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(item_id)])],
        &kw,
    )
    .await;
    assert_eq!(
        trash_rep,
        PyRep::List(Vec::new()),
        "TrashItems full success must return empty list [] on the wire (never [KeyVal]): {trash_rep:?}"
    );
    assert!(
        ctx.db.with_snapshot(|snap| !snap.items.contains_key(&item_id)),
        "Trashed item {item_id} must be removed from inventory"
    );
    eve_testkit::attest(&["rpc:invbroker.TrashItems"]);
}

#[tokio::test]
async fn pin_1187_jump_clone_state_wire_shape() {
    // parity: rpc:jumpCloneSvc.GetCloneState
    // Client: eve/client/script/ui/services/clonejumpsvc.pyj:93 (CloneJump.GetCloneState).
    let ctx = fresh_ctx();
    let clone_id = 300_777;
    let implant_type_id = 9899_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.insert(
            DEMO_CHAR_ID,
            vec![JumpCloneRecord {
                jump_clone_id: clone_id,
                character_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                implants: vec![implant_type_id],
            }],
        );
    });

    let sess = player_sess(
        87_103,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );
    let mut wire = ClientWireSession::from_session(&ctx, sess);
    let kw = HashMap::new();

    let clone_state = wire_session_call_ok(
        &mut wire,
        "jumpCloneSvc",
        None,
        "GetCloneState",
        &[],
        &kw,
    )
    .await;
    let PyRep::KeyVal(kv) = &clone_state else {
        panic!("GetCloneState must return KeyVal even for 1 unnamed clone + implant, got {clone_state:?}");
    };
    let Some(PyRep::Rowset {
        columns: clone_cols,
        rows: clone_rows,
    }) = kv.get("clones")
    else {
        panic!("GetCloneState.clones must be Rowset, got {:?}", kv.get("clones"));
    };
    assert_eq!(clone_cols, &["jumpCloneID", "locationID", "cloneName"]);
    assert_eq!(clone_rows.len(), 1);
    assert_eq!(clone_rows[0][0], PyRep::Integer(clone_id));
    assert_eq!(clone_rows[0][1], PyRep::Integer(JITA_STATION_ID));
    assert_eq!(clone_rows[0][2], PyRep::String(String::new()));

    let Some(PyRep::Rowset {
        columns: imp_cols,
        rows: imp_rows,
    }) = kv.get("implants")
    else {
        panic!(
            "GetCloneState.implants must be Rowset, got {:?}",
            kv.get("implants")
        );
    };
    assert_eq!(imp_cols, &["jumpCloneID", "implantID", "typeID"]);
    assert_eq!(imp_rows.len(), 1);
    assert_eq!(imp_rows[0][0], PyRep::Integer(clone_id));
    assert!(
        imp_rows[0][1].as_i64().is_some_and(|id| id > 0),
        "GetCloneState.implants row must have positive implantID, got {:?}",
        imp_rows[0]
    );
    assert_eq!(imp_rows[0][2], PyRep::Integer(i64::from(implant_type_id)));
    assert!(kv.contains_key("timeLastJump"));

    assert_contract_enforcing(&ctx, "jumpCloneSvc.GetCloneState", &clone_state);
    eve_testkit::attest(&["rpc:jumpCloneSvc.GetCloneState"]);
}

#[tokio::test]
async fn pin_1187_alsc_log_get_wire_shape() {
    // parity: rpc:invbroker.ALSCLogGet
    // Client: eve/client/script/ui/shared/AuditLogSecureContainerLogViewer.pyj:147 (ViewLogForContainer).
    let ctx = fresh_ctx();
    let (alsc_id, inner_item_id) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: 17_366, // Audit Log Secure Container
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            InventoryItemRecord {
                item_id: iid,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: DEMO_CHAR_ID,
                location_id: cid,
                flag: InventoryFlag::Cargo,
                quantity: 42,
                singleton: false,
                ..Default::default()
            },
        );
        (cid, iid)
    });

    let sess = player_sess(
        87_104,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );
    let mut wire = ClientWireSession::from_session(&ctx, sess);
    let kw = HashMap::new();

    let bind_rep = wire_session_call_ok(
        &mut wire,
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
    let mgr_bid = extract_bound_id_from_wire_rep(&bind_rep).expect("invbroker mgr bound id");

    let cont_inv = wire_session_call_ok(
        &mut wire,
        "invbroker",
        Some(mgr_bid),
        "GetInventoryFromId",
        &[PyRep::Integer(alsc_id), PyRep::Bool(false)],
        &kw,
    )
    .await;
    let alsc_bid = extract_bound_id_from_wire_rep(&cont_inv).expect("ALSC bound id");

    let _lock_rep = wire_session_call_ok(
        &mut wire,
        "invbroker",
        Some(alsc_bid),
        "ALSCLockItems",
        &[
            PyRep::Integer(alsc_id),
            PyRep::List(vec![PyRep::Integer(inner_item_id)]),
        ],
        &kw,
    )
    .await;

    let log_rep = wire_session_call_ok(
        &mut wire,
        "invbroker",
        Some(alsc_bid),
        "ALSCLogGet",
        &[PyRep::None, PyRep::Integer(1), PyRep::None],
        &kw,
    )
    .await;
    let PyRep::List(rows) = &log_rep else {
        panic!("invbroker.ALSCLogGet must return a List of log rows, got {log_rep:?}");
    };
    assert_eq!(rows.len(), 1, "Expected 1 audit log entry after locking item: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.dict_get("logID").and_then(PyRep::as_i64), Some(1));
    assert!(
        row.dict_get("logDate")
            .and_then(PyRep::as_i64)
            .is_some_and(|ft| ft >= 100_000_000_000_000),
        "logDate must be Windows FILETIME ticks: {row:?}"
    );
    assert_eq!(row.dict_get("actionID").and_then(PyRep::as_i64), Some(7));
    assert_eq!(
        row.dict_get("actorID").and_then(PyRep::as_i64),
        Some(DEMO_CHAR_ID)
    );
    assert_eq!(
        row.dict_get("arg1").and_then(PyRep::as_i64),
        Some(i64::from(TRITANIUM_TYPE_ID))
    );
    assert_eq!(row.dict_get("arg2").and_then(PyRep::as_i64), Some(42));
    assert_eq!(row.dict_get("flagID").and_then(PyRep::as_i64), Some(4));
    assert_eq!(
        row.dict_get("locationID").and_then(PyRep::as_i64),
        Some(JITA_STATION_ID)
    );
    assert_eq!(
        row.dict_get("inventoryMgrLocationID")
            .and_then(PyRep::as_i64),
        Some(JITA_STATION_ID)
    );
    assert_eq!(row.dict_get("status").and_then(PyRep::as_i64), Some(1));

    assert_contract_enforcing(&ctx, "invbroker.ALSCLogGet", &log_rep);
    eve_testkit::attest(&["rpc:invbroker.ALSCLogGet"]);
}

#[tokio::test]
async fn pin_1187_paperdoll_portrait_data_wire_shape() {
    // parity: rpc:paperDollServer.GetPaperDollPortraitDataFor
    // Client: eve/client/script/ui/services/ccSvc.pyj:180 (GetPortraitData),
    //         packages/evephotosvc/scenePortraitRenderer.pyj:61 (GetPortraitSettings).
    let ctx = fresh_ctx();
    let sess_pre = player_sess(87_105, None, None, None, None);
    let mut wire = ClientWireSession::from_session(&ctx, sess_pre);
    let kw = HashMap::new();

    let doll_kv = PyRep::keyval([
        ("gender", PyRep::Integer(1)),
        ("bloodlineID", PyRep::Integer(1)),
    ]);
    let portrait_kv = PyRep::keyval([
        ("backgroundID", PyRep::Integer(42)),
        ("lightID", PyRep::Integer(10_866)),
        ("lightColorID", PyRep::Integer(10_868)),
        ("lightIntensity", PyRep::Real(0.75)),
        ("portraitPoseNumber", PyRep::Integer(7)),
        (
            "cameraPosition",
            PyRep::Tuple(vec![
                PyRep::Real(0.11),
                PyRep::Real(1.55),
                PyRep::Real(1.31),
            ]),
        ),
        (
            "cameraPoi",
            PyRep::Tuple(vec![
                PyRep::Real(0.02),
                PyRep::Real(1.62),
                PyRep::Real(0.05),
            ]),
        ),
        ("cameraFieldOfView", PyRep::Real(0.38)),
        (
            "headLookTarget",
            PyRep::Tuple(vec![
                PyRep::Real(0.01),
                PyRep::Real(1.52),
                PyRep::Real(0.95),
            ]),
        ),
        (
            "poseData",
            PyRep::keyval([
                ("HeadTilt", PyRep::Real(0.25)),
                ("SmileLeft", PyRep::Real(0.4)),
            ]),
        ),
    ]);

    let new_cid_rep = wire_session_call_ok(
        &mut wire,
        "charUnboundMgr",
        None,
        "CreateCharacterWithDoll",
        &[
            PyRep::String("PortraitPinPilot".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            doll_kv,
            portrait_kv,
            PyRep::Integer(17),
        ],
        &kw,
    )
    .await;
    let new_cid = new_cid_rep.as_i64().expect("created character ID");

    let _sel = wire_session_call_ok(
        &mut wire,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(new_cid)],
        &kw,
    )
    .await;

    // Uploading portrait JPEG bytes must not overwrite saved camera/light portrait metadata.
    let _upload = wire_session_call_ok(
        &mut wire,
        "photoUploadSvc", // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
        None,
        "Upload",
        &[
            PyRep::Integer(new_cid),
            PyRep::Bytes(vec![0xFF, 0xD8, 0xFF, 0xE0]),
        ],
        &kw,
    )
    .await;

    // Case (a): character with stored portrait data returns ([DBRow], None) and passes contract validator.
    let populated_rep = wire_session_call_ok(
        &mut wire,
        "paperDollServer",
        None,
        "GetPaperDollPortraitDataFor",
        &[PyRep::Integer(new_cid)],
        &kw,
    )
    .await;
    let PyRep::Tuple(parts_a) = &populated_rep else {
        panic!("GetPaperDollPortraitDataFor must return 2-tuple, got {populated_rep:?}");
    };
    assert_eq!(parts_a.len(), 2);
    let PyRep::List(rows_a) = &parts_a[0] else {
        panic!("GetPaperDollPortraitDataFor[0] must be List, got {:?}", parts_a[0]);
    };
    assert_eq!(rows_a.len(), 1);
    let PyRep::DBRow { columns, .. } = &rows_a[0] else {
        panic!("GetPaperDollPortraitDataFor[0][0] must be DBRow, got {:?}", rows_a[0]);
    };
    let col = |name: &str| {
        columns
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("missing column `{name}` in portrait DBRow"))
    };
    assert_eq!(col("backgroundID").as_i64(), Some(42));
    assert_eq!(col("lightID").as_i64(), Some(10_866));
    assert_eq!(col("lightColorID").as_i64(), Some(10_868));
    assert!((col("lightIntensity").as_f64().unwrap() - 0.75).abs() < 1e-6);
    assert_eq!(col("portraitPoseNumber").as_i64(), Some(7));
    assert!((col("cameraX").as_f64().unwrap() - 0.11).abs() < 1e-6);
    assert!((col("cameraPoiY").as_f64().unwrap() - 1.62).abs() < 1e-6);
    assert!((col("cameraFieldOfView").as_f64().unwrap() - 0.38).abs() < 1e-6);
    assert!((col("headLookTargetX").as_f64().unwrap() - 0.01).abs() < 1e-6);
    assert!((col("headTilt").as_f64().unwrap() - 0.25).abs() < 1e-6);
    assert!((col("smileLeft").as_f64().unwrap() - 0.4).abs() < 1e-6);
    assert_contract_enforcing(
        &ctx,
        "paperDollServer.GetPaperDollPortraitDataFor",
        &populated_rep,
    );

    // Case (b): character without stored portrait data returns ([], None).
    assert!(
        !ctx.db
            .with_snapshot(|s| s.character_doll_info.contains_key(&DEMO_CHAR_ID)),
        "DEMO_CHAR_ID must have no character_doll_info in fresh demo seed"
    );
    let empty_rep = wire_session_call_ok(
        &mut wire,
        "paperDollServer",
        None,
        "GetPaperDollPortraitDataFor",
        &[PyRep::Integer(DEMO_CHAR_ID)],
        &kw,
    )
    .await;
    assert_eq!(
        empty_rep,
        PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::None]),
        "GetPaperDollPortraitDataFor without stored portrait data must return ([], None)"
    );

    eve_testkit::attest(&["rpc:paperDollServer.GetPaperDollPortraitDataFor"]);
}

#[tokio::test]
async fn pin_933_t2_migrated_rpc_wire_shapes() {
    let ctx = fresh_ctx();
    let now_unix = ctx.now_unix();
    let mut sess = player_sess(
        87_201,
        Some(DEMO_CHAR_ID),
        Some(DEMO_CORP_ID),
        Some(JITA_SYSTEM_ID),
        Some(JITA_STATION_ID),
    );
    sess.role |= service_roles::ROLE_ADMIN;

    let jita_sys = ctx
        .sde
        .solar_systems
        .get(&JITA_SYSTEM_ID)
        .expect("Jita solar system in SDE");
    let jita_constellation_id = jita_sys.constellation_id;
    let jita_region_id = jita_sys.region_id;
    let jita_planet_id = *jita_sys
        .planet_ids
        .first()
        .expect("Jita must have at least one planet in SDE");
    let sample_agent_id = *ctx.sde.agents.keys().min().expect("at least one SDE agent");
    let sample_npc_corp_id = *ctx
        .sde
        .npc_corporations
        .keys()
        .min()
        .expect("at least one SDE NPC corporation");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.corporation_id = DEMO_CORP_ID;
            ch.standings.insert(sample_agent_id, 2.5);
        }
        snap.alliances.insert(
            99_933_001,
            eve_db::AllianceRecord {
                alliance_id: 99_933_001,
                name: "Pin Sov".into(),
                short_name: "PSOV".into(),
                executor_corp_id: DEMO_CORP_ID,
                member_corp_ids: [DEMO_CORP_ID].into_iter().collect(),
            },
        );
        let sys_st = snap
            .system_states
            .entry(JITA_SYSTEM_ID)
            .or_insert_with(|| eve_db::SystemDynamicState {
                solar_system_id: JITA_SYSTEM_ID,
                ..Default::default()
            });
        sys_st.sov_alliance_id = Some(99_933_001);
        sys_st.insurgency_corruption_stage = 5;

        snap.structures.insert(
            1_093_300_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_093_300_001,
                type_id: eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: DEMO_CORP_ID,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Pin Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_093_300_002,
            eve_db::UpwellStructureRecord {
                structure_id: 1_093_300_002,
                type_id: 12_235,
                owner_corp_id: DEMO_CORP_ID,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Pin Tower".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (DEMO_CHAR_ID, jita_planet_id),
            eve_db::PlanetaryColonyRecord {
                planet_id: jita_planet_id,
                owner_id: DEMO_CHAR_ID,
                solar_system_id: JITA_SYSTEM_ID,
                command_center_level: 2,
                pins_count: 1,
                last_extracted_unix: now_unix,
                extracted_commodities: HashMap::new(),
            },
        );
        snap.industry_jobs.insert(
            93_301,
            eve_db::IndustryJobRecord {
                job_id: 93_301,
                installer_id: DEMO_CHAR_ID,
                facility_id: JITA_STATION_ID,
                blueprint_item_id: 88_001,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 1,
                product_type_id: RIFTER_TYPE_ID,
                status: "Active".into(),
                duration_seconds: 600,
                start_unix: now_unix,
                blueprint_location_id: JITA_STATION_ID,
                output_location_id: JITA_STATION_ID,
                ..Default::default()
            },
        );
    });

    let mut wire = ClientWireSession::from_session(&ctx, sess);
    let kw = HashMap::new();

    // 1. config.GetMultiCorpTickerNamesEx -> native CRowset (reply_shape is null in expectations).
    let tickers = wire_session_call_ok(
        &mut wire,
        "config",
        None,
        "GetMultiCorpTickerNamesEx",
        &[PyRep::List(vec![PyRep::Integer(sample_npc_corp_id)])],
        &kw,
    )
    .await;
    assert!(tickers.is_crowset(), "expected CRowset from GetMultiCorpTickerNamesEx, got {tickers:?}");
    let ticker_rows = tickers.crowset_rows().expect("crowset rows");
    assert_eq!(ticker_rows.len(), 1);
    assert_eq!(
        ticker_rows[0].dbrow_get("corporationID"),
        Some(&PyRep::Integer(sample_npc_corp_id))
    );
    assert!(
        ticker_rows[0]
            .dbrow_get("tickerName")
            .and_then(PyRep::as_str)
            .is_some_and(|s| !s.is_empty()),
        "expected non-empty tickerName in DBRow: {:?}",
        ticker_rows[0]
    );
    for col in ["shape1", "shape2", "shape3", "color1", "color2", "color3"] {
        assert!(ticker_rows[0].dbrow_get(col).is_some(), "missing ticker column {col}");
    }

    // 2. map.GetConstellationLPData
    let lp_data = wire_session_call_ok(
        &mut wire,
        "map",
        None,
        "GetConstellationLPData",
        &[PyRep::Integer(i64::from(jita_constellation_id))],
        &kw,
    )
    .await;
    let PyRep::Rowset { columns: lp_cols, rows: lp_rows } = &lp_data else {
        panic!("expected Rowset from map.GetConstellationLPData, got {lp_data:?}");
    };
    assert_eq!(lp_cols, &vec!["solarSystemID".to_string(), "lpFactor".to_string()]);
    assert!(!lp_rows.is_empty());
    assert_contract_enforcing(&ctx, "map.GetConstellationLPData", &lp_data);

    // 3. map.GetCurrentSovData
    let sov_data = wire_session_call_ok(
        &mut wire,
        "map",
        None,
        "GetCurrentSovData",
        &[PyRep::Integer(i64::from(jita_region_id))],
        &kw,
    )
    .await;
    assert_eq!(sov_data.row_count(), Some(1));
    assert_contract_enforcing(&ctx, "map.GetCurrentSovData", &sov_data);

    // 4. map.GetMyExtraMapInfoAgents
    let extra_agents = wire_session_call_ok(
        &mut wire,
        "map",
        None,
        "GetMyExtraMapInfoAgents",
        &[],
        &kw,
    )
    .await;
    assert_eq!(extra_agents.row_count(), Some(1));
    assert_contract_enforcing(&ctx, "map.GetMyExtraMapInfoAgents", &extra_agents);

    // 5. map.GetSecurityModifiedSystems (populated corruption stage 5 system)
    let sec_mod = wire_session_call_ok(
        &mut wire,
        "map",
        None,
        "GetSecurityModifiedSystems",
        &[],
        &kw,
    )
    .await;
    assert!(sec_mod.row_count().is_some_and(|n| n >= 1));
    assert_contract_enforcing(&ctx, "map.GetSecurityModifiedSystems", &sec_mod);

    // 6. planetMgr.GetPlanetsForChar
    let planets = wire_session_call_ok(&mut wire, "planetMgr", None, "GetPlanetsForChar", &[], &kw).await;
    assert!(matches!(planets, PyRep::List(ref v) if !v.is_empty()));
    assert_contract_enforcing(&ctx, "planetMgr.GetPlanetsForChar", &planets);

    // 7. planetMgr.GMGetSynchedServerState
    let gm_sync = wire_session_call_ok(
        &mut wire,
        "planetMgr",
        None,
        "GMGetSynchedServerState",
        &[PyRep::Integer(jita_planet_id)],
        &kw,
    )
    .await;
    assert_contract_enforcing(&ctx, "planetMgr.GMGetSynchedServerState", &gm_sync);

    // 8. industryManager.GetJob
    let job = wire_session_call_ok(
        &mut wire,
        "industryManager",
        None,
        "GetJob",
        &[PyRep::Integer(93_301)],
        &kw,
    )
    .await;
    assert_eq!(job.dict_get("jobID"), Some(&PyRep::Integer(93_301)));
    assert_eq!(job.dict_get("status"), Some(&PyRep::Integer(1)));
    assert_contract_enforcing(&ctx, "industryManager.GetJob", &job);

    // 9. industryManager.GetJobsByOwner
    let jobs = wire_session_call_ok(
        &mut wire,
        "industryManager",
        None,
        "GetJobsByOwner",
        &[PyRep::Integer(DEMO_CHAR_ID)],
        &kw,
    )
    .await;
    assert!(matches!(jobs, PyRep::List(ref v) if v.len() == 1));
    assert_contract_enforcing(&ctx, "industryManager.GetJobsByOwner", &jobs);

    // 10. posMgr.GetControlTowers
    let towers = wire_session_call_ok(&mut wire, "posMgr", None, "GetControlTowers", &[], &kw).await;
    assert!(matches!(towers, PyRep::List(ref v) if !v.is_empty()));
    assert_contract_enforcing(&ctx, "posMgr.GetControlTowers", &towers);

    // 11. charMgr.GetOwnerNoteLabels
    let note_id = wire_session_call_ok(
        &mut wire,
        "charMgr",
        None,
        "AddOwnerNote",
        &[
            PyRep::Integer(DEMO_CHAR_ID),
            PyRep::String("N:Tactical".into()),
            PyRep::String("Hold gate".into()),
        ],
        &kw,
    )
    .await;
    assert!(
        note_id.as_i64().is_some_and(|id| id > 0),
        "AddOwnerNote must return positive noteID: {note_id:?}"
    );
    let note_labels = wire_session_call_ok(&mut wire, "charMgr", None, "GetOwnerNoteLabels", &[], &kw).await;
    assert!(matches!(note_labels, PyRep::List(ref v) if v.len() == 1));
    assert_contract_enforcing(&ctx, "charMgr.GetOwnerNoteLabels", &note_labels);

    // 12. charMgr.GetContactList (addresses populated; blocked remains empty matching native Rowset)
    let added = wire_session_call_ok(
        &mut wire,
        "charMgr",
        None,
        "AddContact",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Real(5.0),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .await;
    assert_eq!(
        added.dict_get("contactID"),
        Some(&PyRep::Integer(90_000_002)),
        "AddContact must return added contactID KeyVal: {added:?}"
    );
    let contacts = wire_session_call_ok(&mut wire, "charMgr", None, "GetContactList", &[], &kw).await;
    assert_eq!(
        contacts.dict_get("addresses").and_then(PyRep::row_count),
        Some(1),
        "GetContactList.addresses must have 1 contact: {contacts:?}"
    );
    let raw_exp = std::fs::read_to_string(ClientContractRatchet::default_expectations_path())
        .expect("read client_expectations.json");
    let root_exp: serde_json::Value =
        serde_json::from_str(&raw_exp).expect("parse client_expectations.json");
    let contact_shape = root_exp["rpc"]["charMgr.GetContactList"]["reply_shape"]
        .as_object()
        .expect("charMgr.GetContactList reply_shape");
    let sem = SemanticLookupContext {
        sde: &ctx.sde,
        db: &ctx.db,
    };
    let contact_outcome = validate_reply_shape_with_metrics(
        Some(&sem),
        "charMgr.GetContactList",
        &contacts,
        contact_shape,
    );
    assert!(
        contact_outcome.violations.is_empty(),
        "charMgr.GetContactList contract violations: {:#?}",
        contact_outcome.violations
    );
    assert!(contact_outcome.paths_checked > 0);

    eve_testkit::attest(&[
        "rpc:config.GetMultiCorpTickerNamesEx",
        "rpc:map.GetConstellationLPData",
        "rpc:map.GetCurrentSovData",
        "rpc:map.GetMyExtraMapInfoAgents",
        "rpc:map.GetSecurityModifiedSystems",
        "rpc:planetMgr.GetPlanetsForChar",
        "rpc:planetMgr.GMGetSynchedServerState",
        "rpc:industryManager.GetJob",
        "rpc:industryManager.GetJobsByOwner",
        "rpc:posMgr.GetControlTowers",
        "rpc:charMgr.GetOwnerNoteLabels",
        "rpc:charMgr.GetContactList",
    ]);
}
