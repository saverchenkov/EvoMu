//! Character management, standings, clone bays, and clone jump integration tests.

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s4_split_shared_handlers_and_jump_clones() {
    // parity: plan:S-4, rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.SelectCharacterID, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.GetCloneState, rpc:jumpCloneSvc.GetPriceForClone, rpc:jumpCloneSvc.GetStationCloneState, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure, rpc:loginRewardFacilities.get_my_clone_grade, rpc:machoNet.GetServiceInfo, rpc:standingMgr.GetCharStandings, rpc:standingMgr.GetStandingTransactions, rpc:subscriptionMgr.GetCloneGrade
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(904);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. charUnboundMgr: GetCharactersToSelect vs GetCharacterInfo(char_id)
    let chars_list = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharactersToSelect");
    assert!(chars_list.is_crowset());
    let char_info_missing_arg = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        char_info_missing_arg.unwrap_err().code,
        "CharacterNotSelected"
    );
    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterInfo(90_000_001)");
    assert!(matches!(char_info, PyRep::KeyVal(_)));

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 2. standingMgr: GetCharStandings vs GetStandingTransactions
    let standings_rs = ctx
        .dispatch_rpc(
            &mut sess,
            "standingMgr",
            None,
            "GetCharStandings",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharStandings");
    let PyRep::Rowset {
        columns: st_hdr, ..
    } = standings_rs
    else {
        panic!("Expected Rowset from GetCharStandings");
    };
    assert_eq!(st_hdr, vec!["fromID", "standing"]);

    let st_tx_rs = ctx
        .dispatch_rpc(
            &mut sess,
            "standingMgr",
            None,
            "GetStandingTransactions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions");
    let PyRep::Rowset {
        columns: tx_hdr, ..
    } = st_tx_rs
    else {
        panic!("Expected Rowset from GetStandingTransactions");
    };
    assert_eq!(
        tx_hdr,
        vec!["eventID", "fromID", "toID", "modification", "eventTypeID"]
    );

    // 3. machoNet: get_init_vals vs GetServiceInfo
    let init_vals = eve_server::services::auth_cache::get_init_vals(&ctx);
    assert!(matches!(init_vals, PyRep::Tuple(_)));
    let svc_info = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            None,
            "GetServiceInfo",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetServiceInfo");
    assert!(matches!(svc_info, PyRep::Dict(_)));

    // 4. subscriptionMgr / loginRewardFacilities: GetCloneGrade vs get_my_clone_grade
    let clone_grade_kv = ctx
        .dispatch_rpc(
            &mut sess,
            "subscriptionMgr",
            None,
            "GetCloneGrade",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCloneGrade");
    assert!(matches!(clone_grade_kv, PyRep::KeyVal(_)));
    let clone_grade_int = ctx
        .dispatch_rpc(
            &mut sess,
            "loginRewardFacilities",
            None,
            "get_my_clone_grade",
            &[],
            &kw,
            None,
        )
        .await
        .expect("get_my_clone_grade");
    assert_eq!(clone_grade_int, PyRep::Integer(2));

    // 5. jumpCloneSvc: GetPriceForClone, InstallCloneInStation (station clone bay check),
    // InstallCloneInStructure, GetCloneState vs GetStationCloneState, CloneJump (implant swap + departure bay check + cooldown)
    let price_resp = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetPriceForClone",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPriceForClone");
    assert_eq!(price_resp, PyRep::Real(900_000.0));

    let struct_not_docked = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(struct_not_docked.unwrap_err().code, "MustBeDocked");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_000_361); // Jita IV - Moon 6 - Ytiri Storage (operation_id 26, no clone bay)
        }
    });
    let no_bay_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(no_bay_err.unwrap_err().code, "StationHasNoCloneBay");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    let jc_install = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStation at 60003760");
    let PyRep::KeyVal(jc_kv) = jc_install else {
        panic!("Expected KeyVal from InstallCloneInStation");
    };
    let jc_id = jc_kv.get("jumpCloneID").unwrap().as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(90_000_001, vec![9899]);
        if let Some(clones) = snap.jump_clones.get_mut(&90_000_001) {
            if let Some(c) = clones.iter_mut().find(|c| c.jump_clone_id == jc_id) {
                c.location_id = 60_008_494;
                c.implants = vec![9941];
            }
        }
    });

    // Attempting CloneJump while docked at no-bay station 60000361 must reject so no clone is left at a bay-less station
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_000_361);
        }
    });
    let jump_from_no_bay = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(jump_from_no_bay.unwrap_err().code, "StationHasNoCloneBay");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
    });
    let state_all = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetCloneState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCloneState");
    let PyRep::Rowset { rows: all_rows, .. } = state_all else {
        panic!("Expected Rowset from GetCloneState");
    };
    assert_eq!(all_rows.len(), 1);
    let state_station = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetStationCloneState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetStationCloneState");
    let PyRep::Rowset {
        rows: station_rows, ..
    } = state_station
    else {
        panic!("Expected Rowset from GetStationCloneState");
    };
    assert_eq!(station_rows.len(), 0);

    ctx.advance_secs(12);
    let pod_id = 99_000_600;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, eve_db::InventoryItemRecord { item_id: pod_id, type_id: 670, owner_id: 90_000_001, location_id: 60_003_760, flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default() });
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = pod_id;
        }
    });
    sess.ship_id = Some(pod_id);
    let jump_swap = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump implant swap");
    let PyRep::KeyVal(js_kv) = jump_swap else {
        panic!("Expected KeyVal from CloneJump");
    };
    assert_eq!(
        js_kv.get("activeImplants"),
        Some(&PyRep::List(vec![PyRep::Integer(9941)]))
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_implants.get(&90_000_001), Some(&vec![9941]));
        let left_behind = snap.jump_clones[&90_000_001]
            .iter()
            .find(|c| c.jump_clone_id == jc_id)
            .unwrap();
        assert_eq!(left_behind.location_id, 60_003_760);
        assert_eq!(left_behind.implants, vec![9899]);
        let ch = &snap.characters[&90_000_001];
        assert_eq!(ch.station_id, Some(60_008_494));
        assert_eq!(ch.solar_system_id, 30_002_187);
    });

    ctx.advance_secs(12);
    let cooldown_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(cooldown_err.unwrap_err().code, "CloneJumpCooldownActive");
}

#[tokio::test]
async fn test_p1_p2_wire_row_encoders_and_service_crowsets() {
    // parity: plan:P-1, plan:P-2, feat:serverops.packed-rows, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.GetContainerContents, rpc:invbroker.GetItemDescriptor, rpc:marketProxy.GetOrders, rpc:marketProxy.GetStationAsks
    use eve_proto::{marshal, DBRowDescriptor};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();
    let mut sess = SessionState::new(905);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. charUnboundMgr.GetCharactersToSelect returns dbutil.CRowset of packed DBRows that round-trip through marshal
    let chars_crowset = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharactersToSelect");
    assert!(chars_crowset.is_crowset());
    let chars_wire = marshal::encode(&chars_crowset);
    let chars_decoded = marshal::decode(&chars_wire).expect("decode GetCharactersToSelect CRowset");
    assert_eq!(chars_decoded, chars_crowset);
    let rows = chars_decoded.crowset_rows().expect("crowset rows");
    assert!(!rows.is_empty());
    assert_eq!(
        rows[0].dbrow_get("characterID"),
        Some(&PyRep::Integer(90_000_001))
    );
    assert_eq!(
        rows[0].dbrow_get("characterName"),
        Some(&PyRep::String("Aura Capsuleer".into()))
    );
    assert_eq!(
        rows[0].dbrow_get("shipTypeID"),
        Some(&PyRep::Integer(587))
    );
    assert_eq!(
        rows[0].dbrow_get("shipName"),
        Some(&PyRep::String("Rifter".into()))
    );

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 2. invbroker.GetItemDescriptor and invbroker.List / GetContainerContents return canonical DBRowDescriptor & dbutil.CRowset
    let item_desc_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetItemDescriptor",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetItemDescriptor");
    let parsed_desc = DBRowDescriptor::from_pyrep(&item_desc_rep).expect("parse DBRowDescriptor");
    assert_eq!(parsed_desc, DBRowDescriptor::inv_item_header());

    // invbroker.List uses the bound inventory location (or active station/ship) and optional flag filter
    let inv_list = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "List",
            &[PyRep::Integer(4)], // Hangar flag filter
            &kw,
            None,
        )
        .await
        .expect("invbroker.List");
    assert!(inv_list.is_crowset());
    let inv_wire = marshal::encode(&inv_list);
    let inv_decoded = marshal::decode(&inv_wire).expect("decode invbroker.List CRowset");
    assert_eq!(inv_decoded, inv_list);
    let inv_rows = inv_decoded.crowset_rows().unwrap();
    assert!(!inv_rows.is_empty());
    assert!(inv_rows[0].dbrow_get("itemID").is_some());
    assert!(inv_rows[0].dbrow_get("groupID").is_some());
    assert!(inv_rows[0].dbrow_get("categoryID").is_some());
    assert_eq!(
        inv_rows[0].dbrow_get("customInfo"),
        Some(&PyRep::String(String::new()))
    );

    // invbroker.GetContainerContents requires a real owned/accessible container item
    let container_contents = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("invbroker.GetContainerContents on owned ship");
    assert!(container_contents.is_crowset());

    // 3. marketProxy.GetOrders returns a (sell_crowset, buy_crowset) 2-tuple of dbutil.CRowsets
    let orders_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(34)],
            &kw,
            None,
        )
        .await
        .expect("marketProxy.GetOrders");
    let orders_wire = marshal::encode(&orders_rep);
    let orders_decoded = marshal::decode(&orders_wire).expect("decode marketProxy.GetOrders");
    assert_eq!(orders_decoded, orders_rep);
    let PyRep::Tuple(order_pair) = orders_decoded else {
        panic!("Expected (sell, buy) Tuple from marketProxy.GetOrders");
    };
    assert_eq!(order_pair.len(), 2);
    assert!(order_pair[0].is_crowset());
    assert!(order_pair[1].is_crowset());
    let sell_rows = order_pair[0].crowset_rows().unwrap();
    assert!(!sell_rows.is_empty());
    assert_eq!(sell_rows[0].dbrow_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(sell_rows[0].dbrow_get("bid"), Some(&PyRep::Bool(false)));

    // 4. marketProxy.GetStationAsks and GetRegionBest return dbutil.CIndexedRowset keyed by typeID
    for method in ["GetStationAsks", "GetRegionBest"] {
        let asks = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, method, &[], &kw, None)
            .await
            .expect(method);
        assert!(asks.is_cindexed_rowset());
        let asks_wire = marshal::encode(&asks);
        assert_eq!(marshal::decode(&asks_wire).unwrap(), asks);
        assert!(!asks.cindexed_rowset_entries().unwrap().is_empty());
    }

    // Undocked calls to GetStationAsks fail cleanly with NotDockedAtStation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.station_id = None;
        }
    });
    sess.station_id = None;
    let undocked_err = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetStationAsks", &[], &kw, None)
        .await
        .expect_err("GetStationAsks requires station_id");
    assert_eq!(undocked_err.code, "NotDockedAtStation");
}

