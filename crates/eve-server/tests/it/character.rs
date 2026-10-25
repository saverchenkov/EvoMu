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

#[tokio::test]
async fn test_r50_p1_1_container_contents_ownership_and_range_enforcement() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.GetContainerContents
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(981);
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    // Put character 90_000_001 in space in Jita (30000142) aboard ship 100_000_001 at (0, 0, 0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.station_id = None;
            c.solar_system_id = 30_000_142;
        }
        // 1) Another player's ship in Jita (category 6, type 587) -> must NEVER be inspectable
        snap.items.insert(
            990_000_001,
            eve_db::InventoryItemRecord {
                item_id: 990_000_001,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // 2) Cargo container (type 23, group 12) in another solar system (Perimeter = 30000144)
        snap.items.insert(
            990_000_002,
            eve_db::InventoryItemRecord {
                item_id: 990_000_002,
                type_id: 23,
                owner_id: 90_000_002,
                location_id: 30_000_144,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // 3) Cargo container (type 23, group 12) in Jita (30000142)
        snap.items.insert(
            990_000_003,
            eve_db::InventoryItemRecord {
                item_id: 990_000_003,
                type_id: 23,
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // Loot item inside container 990_000_003
        snap.items.insert(
            990_000_004,
            eve_db::InventoryItemRecord {
                item_id: 990_000_004,
                type_id: 34,
                owner_id: 90_000_002,
                location_id: 990_000_003,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
    });

    let jita = ctx.systems.get_or_spawn(30_000_142);
    jita.spawn_ball(Ball {
        id: 100_000_001,
        type_id: 606,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn player ship");

    jita.spawn_ball(Ball {
        id: 990_000_001,
        type_id: 587,
        position: DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn other player ship");

    // 1. Another player's in-space ship is rejected even at 500m
    let ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_001)],
            &kw,
            None,
        )
        .await
        .expect_err("another player's ship must not be inspectable");
    assert_eq!(ship_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 2. Container in another solar system (30000144) is rejected
    let sys_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_002)],
            &kw,
            None,
        )
        .await
        .expect_err("container in another system must be rejected");
    assert_eq!(sys_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 3. Container in same solar system but 10,000m away (> 2,500m) is rejected
    jita.spawn_ball(Ball {
        id: 990_000_003,
        type_id: 23,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn far container");

    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_003)],
            &kw,
            None,
        )
        .await
        .expect_err("container beyond 2,500m must be rejected");
    assert_eq!(range_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 4. Move container within 1,500m (<= 2,500m) in same system -> accepted!
    jita.remove_ball(990_000_003).await.expect("remove far ball");
    jita.spawn_ball(Ball {
        id: 990_000_003,
        type_id: 23,
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn near container");

    let near_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_003)],
            &kw,
            None,
        )
        .await
        .expect("in-range cargo container in same system should succeed");
    assert!(near_ok.is_crowset());
    let rows = near_ok.crowset_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(990_000_004)));
}

#[tokio::test]
async fn test_p3_p4_p5_object_cache_config_service_and_notifications() {
    // parity: plan:P-3, plan:P-4, plan:P-5, feat:serverops.object-cache, feat:serverops.config-svc, feat:serverops.notification-transport, feat:social.notifications-list, feat:social.notifications-mark, feat:social.notifications-delete, feat:social.notifications-generate, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:config.GetStationSolarSystemsByOwner, rpc:notificationMgr.DeleteNotifications, rpc:notificationMgr.GetAllNotifications, rpc:notificationMgr.GetByGroupID, rpc:notificationMgr.GetUnprocessed, rpc:notificationMgr.MarkAsProcessed
    use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
    use eve_proto::{marshal, DBRowDescriptor};
    use eve_server::router::NotificationTarget;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(982);
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    // ========================================================================
    // P-3: objectCaching.GetCachableObject, CachedMethodCallResult, Invalidation
    // ========================================================================
    let units_oid = PyRep::String("config.Units".into());
    let cached_units_obj = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "GetCachableObject",
            &[
                PyRep::Bool(false),
                units_oid.clone(),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Integer(888_444),
            ],
            &kw,
            None,
        )
        .await
        .expect("objectCaching.GetCachableObject(config.Units)");

    // Verify wire round-trip of objectCaching.CachedObject and inner marshaled payload
    let co_wire = marshal::encode(&cached_units_obj);
    let co_decoded = marshal::decode(&co_wire).expect("decode objectCaching.CachedObject");
    assert_eq!(co_decoded, cached_units_obj);
    let (ts1, ver1) = match &co_decoded {
        PyRep::Object { type_name, args } => {
            assert_eq!(type_name, "objectCaching.CachedObject");
            let PyRep::Tuple(items) = args.as_ref() else {
                panic!("Expected 7-tuple in objectCaching.CachedObject");
            };
            assert_eq!(items.len(), 7);
            let PyRep::Tuple(ver_pair) = &items[0] else {
                panic!("Expected (timestamp, version) tuple");
            };
            let ts = ver_pair[0].as_i64().unwrap();
            let ver = ver_pair[1].as_i64().unwrap();
            let PyRep::Buffer(inner_bytes) = &items[4] else {
                panic!("Expected Buffer in CachedObject lret");
            };
            let inner_rep = marshal::decode(inner_bytes).expect("decode inner cached payload");
            assert!(matches!(inner_rep, PyRep::IndexRowset { .. }));
            assert_eq!(items[6], units_oid);
            (ts, ver)
        }
        other => panic!("Expected PyRep::Object, got {other:?}"),
    };

    // Verify CachedMethodCallResult inline and hint forms round-trip through marshal
    let wrapped_cmcr = ctx.cache_method_call_result(
        "config",
        "GetStationSolarSystemsByOwner",
        ctx.build_config_units_payload(),
        "run",
        None,
    );
    let cmcr_wire = marshal::encode(&wrapped_cmcr);
    assert_eq!(marshal::decode(&cmcr_wire).unwrap(), wrapped_cmcr);

    // Unknown config.* cache key must fail with CachedObjectNotFound (no empty-rowset fallback)
    let unknown_cache_err = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "GetCachableObject",
            &[
                PyRep::Integer(0),
                PyRep::Tuple(vec![
                    PyRep::String("Method Call".into()),
                    PyRep::String("server".into()),
                    PyRep::Tuple(vec![
                        PyRep::String("config".into()),
                        PyRep::String("GetMapData".into()),
                    ]),
                ]),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unknown cache key must return CachedObjectNotFound");
    assert_eq!(unknown_cache_err.code, "CachedObjectNotFound");

    // Non-GM caller must be rejected on objectCaching.InvalidateCachedObjects
    let non_gm_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "InvalidateCachedObjects",
            &[PyRep::List(vec![units_oid.clone()])],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM InvalidateCachedObjects must fail");
    assert_eq!(non_gm_inv.code, "InsufficientRole");

    // Grant GM role, invalidate, and re-populate with updated payload -> timestamp strictly advances and version changes
    sess.role |= eve_db::service_roles::ROLE_GML;
    let inv_cnt = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "InvalidateCachedObjects",
            &[PyRep::List(vec![units_oid.clone()])],
            &kw,
            None,
        )
        .await
        .expect("InvalidateCachedObjects as GM");
    assert_eq!(inv_cnt, PyRep::Integer(1));
    let rec2 = ctx.put_cached_object(
        units_oid.clone(),
        PyRep::rowset(vec!["unitID"], vec![vec![PyRep::Integer(999)]]),
        true,
    );
    assert!(rec2.timestamp > ts1);
    assert_ne!(rec2.version as i64, ver1);
    let hint_rep = rec2.to_cached_method_call_hint_pyrep("run");
    assert_eq!(marshal::decode(&marshal::encode(&hint_rep)).unwrap(), hint_rep);
    sess.role &= !eve_db::service_roles::ROLE_GML;

    // ========================================================================
    // P-4: config.* RPCs (GetMultiOwnersEx, GetMultiLocationsEx, GetMultiStationEx,
    //      GetMultiInvTypesEx, GetMultiCorpTickerNamesEx, GetStationSolarSystemsByOwner, etc.)
    // ========================================================================
    let owners_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_000_035),
                PyRep::Integer(500_001),
            ])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiOwnersEx");
    if let PyRep::TupleSet { columns, rows } = &owners_rep {
        assert_eq!(
            columns,
            &vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], PyRep::Integer(90_000_001));
    } else {
        panic!("Expected TupleSet from GetMultiOwnersEx");
    }

    // Invalid non-integer element in GetMultiOwnersEx must fail with InvalidArgument
    let bad_owners = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![PyRep::String("not-an-int".into())])],
            &kw,
            None,
        )
        .await
        .expect_err("non-integer in ownerIDs must fail");
    assert_eq!(bad_owners.code, "InvalidArgument");

    let locs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiLocationsEx",
            &[PyRep::List(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(60_003_760),
                PyRep::Integer(100_000_001),
            ])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiLocationsEx");
    if let PyRep::TupleSet { columns, rows } = &locs_rep {
        assert_eq!(
            columns,
            &vec![
                "locationID",
                "locationName",
                "x",
                "y",
                "z",
                "locationNameID"
            ]
        );
        assert_eq!(rows.len(), 3);
    } else {
        panic!("Expected TupleSet from GetMultiLocationsEx");
    }

    let stations_rep = eve_server::services::config_service::get_multi_station_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(60_003_760)])],
        &kw,
    )
    .expect("config_service::get_multi_station_ex");
    if let PyRep::TupleSet { rows, .. } = &stations_rep {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], PyRep::Integer(60_003_760));
        assert_eq!(rows[0][3], PyRep::Integer(30_000_142));
    } else {
        panic!("Expected TupleSet from get_multi_station_ex");
    }

    let inv_types_rep = eve_server::services::config_service::get_multi_inv_types_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(587)])],
        &kw,
    )
    .expect("config_service::get_multi_inv_types_ex");
    let inv_types_wire = marshal::encode(&inv_types_rep);
    let inv_types_decoded = marshal::decode(&inv_types_wire).expect("decode get_multi_inv_types_ex");
    assert_eq!(inv_types_decoded, inv_types_rep);
    if let PyRep::List(rows) = &inv_types_decoded {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dbrow_get("typeID"), Some(&PyRep::Integer(34)));
        assert_eq!(rows[1].dbrow_get("typeID"), Some(&PyRep::Integer(587)));
    } else {
        panic!("Expected List of DBRow from get_multi_inv_types_ex");
    }

    let tickers_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiCorpTickerNamesEx",
            &[PyRep::List(vec![PyRep::Integer(1_000_035)])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiCorpTickerNamesEx");
    if let PyRep::List(rows) = &tickers_rep {
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].dbrow_get("corporationID"),
            Some(&PyRep::Integer(1_000_035))
        );
    } else {
        panic!("Expected List of DBRow from GetMultiCorpTickerNamesEx");
    }

    let owner_stations_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetStationSolarSystemsByOwner",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("config.GetStationSolarSystemsByOwner");
    if let PyRep::Rowset { columns, rows } = &owner_stations_rep {
        assert_eq!(columns, &vec!["stationID", "solarSystemID"]);
        assert!(rows
            .iter()
            .any(|r| r[0] == PyRep::Integer(60_003_760) && r[1] == PyRep::Integer(30_000_142)));
    } else {
        panic!("Expected Rowset from GetStationSolarSystemsByOwner");
    }

    let map_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMap",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("config.GetMap");
    assert!(map_rep.row_count().unwrap_or(0) > 0);

    let map_objs_rep = eve_server::services::config_service::get_map_objects(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .expect("config_service::get_map_objects");
    assert!(map_objs_rep.is_crowset());
    assert!(!map_objs_rep.crowset_rows().unwrap().is_empty());

    let map_conns_rep = eve_server::services::config_service::get_map_connections(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .expect("config_service::get_map_connections");
    assert!(map_conns_rep.row_count().unwrap_or(0) > 0);

    // Unhandled config methods and unimported SDE tables must return UnimplementedRPC (no fabricated rows)
    let graphics_err = eve_server::services::config_service::get_multi_graphics_ex(&ctx, &sess)
        .expect_err("get_multi_graphics_ex must return UnimplementedRPC");
    assert_eq!(graphics_err.code, "UnimplementedRPC");

    let cel_stat_err = eve_server::services::config_service::get_celestial_statistic(&ctx, &sess)
        .expect_err("get_celestial_statistic must return UnimplementedRPC");
    assert_eq!(cel_stat_err.code, "UnimplementedRPC");

    let unhandled_cfg_err = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMapData",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect_err("unhandled config.GetMapData must return UnimplementedRPC");
    assert_eq!(unhandled_cfg_err.code, "UnimplementedRPC");

    // ========================================================================
    // P-5: Binary MachoPacket notifications & notificationMgr service
    // ========================================================================
    // Offline character 90_000_002 must not accumulate queued notifications
    let offline_emitted = ctx.notify_account_change(90_000_002, "cash", 90_000_002, 100.0);
    assert!(offline_emitted.is_empty());

    let _ = ctx.drain_notifications_for_char(90_000_001);
    let emitted_acct = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 2_500_000.0);
    assert_eq!(emitted_acct.len(), 1);
    let corp_id = ctx.db.get_character(90_000_001).unwrap().corporation_id;
    let _ = ctx.notify_remote_message(
        NotificationTarget::Corporation(corp_id),
        "CorpNews",
        vec![("text", PyRep::String("Welcome".into()))],
    );
    let sample_item_row = DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(606),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(-1),
        PyRep::Integer(25),
        PyRep::Integer(6),
        PyRep::String(String::new()),
    ]);
    let _ = ctx.notify_item_change(90_000_001, sample_item_row, vec![(3, PyRep::Integer(30_000_142))]);
    let _ = ctx.notify_skill_trained(90_000_001, 200_001, 3_300, 4);
    let created_notif = ctx.create_character_notification(
        1_000_035,
        90_000_001,
        14,
        9,
        "bounty: 125000",
    );

    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert_eq!(drained.len(), 5);
    for notif in &drained {
        // Every queued notification must be a real binary-marshaled MachoPacket(Notification = 12)
        let decoded_pyrep =
            marshal::decode(&notif.marshaled_packet).expect("decode binary MachoPacket");
        let pkt = MachoPacket::from_pyrep(&decoded_pyrep).expect("parse MachoPacket");
        assert_eq!(pkt.packet_type, MachoPacketType::Notification);
        assert_eq!(
            pkt.destination,
            MachoAddress::Broadcast {
                broadcast_id: notif.event.clone(),
                id_type: "charid".into(),
                narrowcast: vec![90_000_001],
            }
        );
    }

    // Verify notificationMgr RPCs return dbutil.CRowset with DBRowDescriptor::notification_header()
    let unproc = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("notificationMgr.GetUnprocessed");
    assert!(unproc.is_crowset());
    let unproc_wire = marshal::encode(&unproc);
    assert_eq!(marshal::decode(&unproc_wire).unwrap(), unproc);
    let unproc_rows = unproc.crowset_rows().unwrap();
    assert_eq!(unproc_rows.len(), 1);
    assert_eq!(
        unproc_rows[0].dbrow_get("notificationID"),
        Some(&PyRep::Integer(created_notif.notification_id))
    );
    assert_eq!(
        unproc_rows[0].dbrow_get("processed"),
        Some(&PyRep::Bool(false))
    );
    assert_eq!(
        unproc_rows[0].dbrow_get("data"),
        Some(&PyRep::String("bounty: 125000".into()))
    );

    let by_grp = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "GetByGroupID",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.GetByGroupID");
    assert_eq!(by_grp.crowset_rows().unwrap().len(), 1);

    let marked = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "MarkAsProcessed",
            &[PyRep::List(vec![PyRep::Integer(
                created_notif.notification_id,
            )])],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.MarkAsProcessed");
    assert_eq!(marked, PyRep::Integer(1));

    let unproc_after = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("notificationMgr.GetUnprocessed after MarkAsProcessed");
    assert_eq!(unproc_after.crowset_rows().unwrap().len(), 0);

    let deleted = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "DeleteNotifications",
            &[PyRep::List(vec![PyRep::Integer(
                created_notif.notification_id,
            )])],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.DeleteNotifications");
    assert_eq!(deleted, PyRep::Integer(1));

    let all_after_del = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "GetAllNotifications",
            &[],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.GetAllNotifications after delete");
    assert_eq!(all_after_del.crowset_rows().unwrap().len(), 0);
}



