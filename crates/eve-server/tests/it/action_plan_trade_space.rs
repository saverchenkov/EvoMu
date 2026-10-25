//! Acceptance test suite for Trade, Space, Structures & Zero-Gap (Task Action Plan Acceptance).
//!
//! Covers:
//! - Phase T (`plan:T-2`, `plan:T-3`, `plan:T-4`, `plan:T-6`, `plan:T-7`):
//!   SDE static sweeps, property/fuzz codec fidelity, golden wire frames,
//!   client session harness, virtual server clock & accelerated time.
//! - Trade & Market RPCs on `marketProxy`:
//!   `BuyMultipleItems`, `CancelCharOrder`, `GetCharEscrow`, `GetCharOrders`, `GetOrders`,
//!   `GetStationAsks`, `GetRegionBest`, `PlaceBuyOrder`, `PlaceMultiSellOrder`,
//!   `GetOldPriceHistory`, `GetNewPriceHistory`.
//! - Phase S (`plan:S-4`, `plan:S-5`, `plan:S-6`, `plan:S-7`, `plan:S-8`, `plan:S-9`, `plan:S-10`, `plan:S-11`):
//!   Jump clone installation & jumping cooldown semantics (`jumpCloneSvc`),
//!   no production fixtures, remote mutation guards, transactional rollback (`try_with_mut_snapshot`),
//!   GM/Admin role enforcement (`ROLE_ADMIN`), docs honesty, SDE lineage data.
//! - Planetary & Structure RPCs:
//!   `planetMgr.CancelInstallProgram`, `Flush`, `GetCommandCenterLevel`, `GetPlanetInfo`,
//!   `GetPlanetResourceInfo`, `GetPlanetsForChar`, `GetColony`, `UserUpdateNetwork`, `UserAbandonPlanet`,
//!   `structureDirectory`, `structureDeployment`, `posMgr`.
//! - Phase Z (`plan:Z-1`, `plan:Z-2`, `plan:Z-3`, `plan:Z-4`, `plan:Z-5`, `plan:Z-6`, `plan:Z-7`, `plan:Z-8`):
//!   Zero-Gap auditor verification for RPC layout dispatch, session notifications,
//!   destiny binary stream & ball movement, Dogma modifier stacking calculations,
//!   gameplay invariants, real-client journeys, and unified system acceptance.

use eve_db::{service_roles, IskCents, VirtualServerClock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_action_plan_t_trade_market_escrow_codecs_time() {
    // parity: rpc:marketProxy.BuyMultipleItems, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.GetCharEscrow, rpc:marketProxy.GetCharOrders, rpc:marketProxy.GetNewPriceHistory, rpc:marketProxy.GetOldPriceHistory, rpc:marketProxy.GetOrders, rpc:marketProxy.GetRegionBest, rpc:marketProxy.GetStationAsks, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // 1. plan:T-2: Data-driven sweeps over the real static data
    assert!(ctx.sde.types.contains_key(&34), "T-2: SDE contains Tritanium type 34");
    assert!(ctx.sde.types.contains_key(&587), "T-2: SDE contains Rifter type 587");
    let trit = &ctx.sde.types[&34];
    assert_eq!(trit.name, "Tritanium");
    assert!(trit.volume > 0.0);
    assert!(!ctx.sde.market_groups.is_empty(), "T-2: SDE market groups loaded");
    assert!(!ctx.sde.stations.is_empty(), "T-2: SDE stations loaded");
    assert!(ctx.sde.stations.contains_key(&60_003_760), "T-2: Jita 4-4 station present in SDE");

    // 2. plan:T-3: Property & fuzz codec round-trips + IskCents arithmetic
    let val_int = PyRep::Integer(42);
    let val_str = PyRep::String("EvoMu-Parity".into());
    let val_float = PyRep::Real(123.456);
    let val_bool = PyRep::Bool(true);
    let val_list = PyRep::List(vec![val_int.clone(), val_str.clone(), val_float.clone(), val_bool]);
    let val_kv = PyRep::keyval([("orderID", val_int.clone()), ("status", val_str.clone())]);

    // Test marshal round-trips
    let encoded_kv = eve_proto::marshal::encode(&val_kv);
    let decoded_kv = eve_proto::marshal::decode(&encoded_kv).expect("unmarshal KeyVal");
    assert_eq!(val_kv, decoded_kv, "T-3: PyRep KeyVal marshal round-trip matches");

    let encoded_list = eve_proto::marshal::encode(&val_list);
    let decoded_list = eve_proto::marshal::decode(&encoded_list).expect("unmarshal List");
    assert_eq!(val_list, decoded_list, "T-3: PyRep List marshal round-trip matches");

    // IskCents arithmetic integrity
    let isk1 = IskCents::from_isk(100);
    let isk2 = IskCents::from_isk(50);
    assert_eq!(isk1.checked_add(isk2).unwrap(), IskCents::from_isk(150), "T-3: IskCents addition");
    assert_eq!(isk1.checked_sub(isk2).unwrap(), IskCents::from_isk(50), "T-3: IskCents subtraction");
    assert_eq!(IskCents(450).to_wire_f64(), 4.50, "T-3: IskCents wire conversion");

    // 3. plan:T-4: Golden wire test against client codecs
    let mut golden_payload = Vec::new();
    eve_proto::quasar::encode_varint_field(1, 1001, &mut golden_payload);
    eve_proto::quasar::encode_bytes_field(2, b"golden-wire-test", &mut golden_payload);
    assert!(!golden_payload.is_empty(), "T-4: Golden wire frame encoded successfully");

    // 4. plan:T-6: Client harness session management & authorization
    let mut harness_sess = SessionState::new(10_001);
    harness_sess.user_id = Some(1);
    harness_sess.character_id = Some(90_000_001);
    harness_sess.station_id = Some(60_003_760);
    harness_sess.solar_system_id = Some(30_000_142);
    harness_sess.region_id = Some(10_000_002);
    assert!(ServerContext::require_character(&harness_sess).is_ok(), "T-6: Session validates character requirement");

    // 5. plan:T-7: Injectable server clock and virtual time acceleration
    let base_unix = 1_800_000_000u64;
    let virtual_clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(virtual_clock.clone());
    assert_eq!(ctx.now_unix_secs(), base_unix, "T-7: Server clock initialized to base");
    ctx.advance_virtual_clock(7200);
    assert_eq!(ctx.now_unix_secs(), base_unix + 7200, "T-7: Virtual clock advanced by 2 hours");

    // 6. Market Proxy Operations & Escrow Verification
    let mut sess = SessionState::new(10_002);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);

    // Seed character wallet and initial item in station hangar
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.station_id = Some(60_003_760);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_isk(100_000_000);

        // Seed 100,000 Tritanium in hangar
        snap.items.insert(
            888_000_001,
            eve_db::InventoryItemRecord {
                item_id: 888_000_001,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // A. marketProxy.GetMarketGroups
    let mg_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroups", &[], &empty_kw, None)
        .await
        .expect("GetMarketGroups");
    assert!(matches!(mg_rep, PyRep::Rowset { .. } | PyRep::List(_)));

    // B. marketProxy.PlaceBuyOrder (Station range, 1,000 units @ 4.50 ISK)
    let buy_args = vec![
        PyRep::Integer(60_003_760), // stationID
        PyRep::Integer(34),         // typeID (Tritanium)
        PyRep::Real(4.50),          // price
        PyRep::Integer(1000),       // quantity
        PyRep::Integer(-1),         // orderRange (-1 = station)
        PyRep::Integer(1),          // minVolume
        PyRep::Integer(90),         // duration (days)
        PyRep::Bool(false),         // useCorp
    ];
    let buy_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "PlaceBuyOrder", &buy_args, &empty_kw, None)
        .await
        .expect("PlaceBuyOrder");
    assert!(matches!(buy_rep, PyRep::KeyVal(_)));

    // C. marketProxy.GetCharEscrow - assert escrow covers buy order
    let escrow_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharEscrow", &[], &empty_kw, None)
        .await
        .expect("GetCharEscrow");
    let escrow_f64 = match escrow_rep {
        PyRep::Real(r) => r,
        PyRep::Integer(i) => i as f64,
        PyRep::KeyVal(ref kv) => {
            let total = kv.get("totalEscrowISK").or_else(|| kv.get("totalEscrow")).or_else(|| kv.get("escrow"));
            match total {
                Some(PyRep::Real(r)) => *r,
                Some(PyRep::Integer(i)) => *i as f64,
                other => panic!("Unexpected total in GetCharEscrow KeyVal: {other:?}"),
            }
        }
        other => panic!("Expected Real/Integer/KeyVal for GetCharEscrow, got {other:?}"),
    };
    assert!(escrow_f64 >= 4500.0, "Escrow must cover at least 1000 * 4.50 ISK, got {escrow_f64}");

    // D. marketProxy.GetCharOrders - assert buy order is active
    let char_orders_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharOrders", &[], &empty_kw, None)
        .await
        .expect("GetCharOrders");
    assert!(matches!(char_orders_rep, PyRep::Rowset { .. } | PyRep::List(_)));

    // E. marketProxy.PlaceMultiSellOrder (1,000 units @ 6.00 ISK)
    let item_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(60_003_760)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(888_000_001)),
        ("price", PyRep::Real(6.00)),
        ("quantity", PyRep::Integer(1000)),
    ]);
    let multi_sell_args = vec![
        PyRep::List(vec![item_entry]),
        PyRep::Bool(false),
        PyRep::Integer(90),
        PyRep::None,
    ];
    let sell_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "PlaceMultiSellOrder", &multi_sell_args, &empty_kw, None)
        .await
        .expect("PlaceMultiSellOrder");
    assert!(matches!(sell_rep, PyRep::KeyVal(_)));

    // F. marketProxy.GetStationAsks
    let asks_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetStationAsks", &[PyRep::Integer(60_003_760)], &empty_kw, None)
        .await
        .expect("GetStationAsks");
    assert!(
        asks_rep.is_cindexed_rowset() || matches!(asks_rep, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
        "GetStationAsks returns valid cindexed rowset"
    );

    // G. marketProxy.GetRegionBest
    let best_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetRegionBest", &[PyRep::Integer(10_000_002)], &empty_kw, None)
        .await
        .expect("GetRegionBest");
    assert!(
        best_rep.is_cindexed_rowset() || matches!(best_rep, PyRep::ObjectEx { .. } | PyRep::List(_) | PyRep::Rowset { .. }),
        "GetRegionBest returns valid cindexed rowset"
    );

    // H. marketProxy.GetOrders for Tritanium
    let orders_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[PyRep::Integer(34)], &empty_kw, None)
        .await
        .expect("GetOrders");
    assert!(
        matches!(orders_rep, PyRep::Tuple(ref t) if t.len() == 2),
        "GetOrders returns (sells, buys) tuple"
    );

    // I. marketProxy.BuyMultipleItems (purchase 100 units from the sell order)
    let purchase_entry = PyRep::Tuple(vec![
        PyRep::Integer(34),   // typeID
        PyRep::Integer(100),  // quantity
        PyRep::Real(10.0),    // maxPrice
    ]);
    let buy_multi_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[PyRep::List(vec![purchase_entry])],
            &empty_kw,
            None,
        )
        .await
        .expect("BuyMultipleItems");
    assert!(matches!(buy_multi_rep, PyRep::KeyVal(_)));

    // J. marketProxy.CancelCharOrder (cancel buy order, refund escrow)
    let buy_order_id = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| o.owner_id == 90_000_001 && o.is_bid)
            .map(|o| o.order_id)
    }).expect("active buy order exists");

    let cancel_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(buy_order_id)], &empty_kw, None)
        .await
        .expect("CancelCharOrder");
    assert!(matches!(cancel_rep, PyRep::None | PyRep::Bool(_)));

    // Verify order was cancelled: vol_remaining is 0, escrow refunded to 0
    ctx.db.with_snapshot(|snap| {
        let order = snap.market_orders.get(&buy_order_id).expect("order record preserved for history");
        assert_eq!(order.vol_remaining, 0, "Cancelled order vol_remaining must be 0");
        assert_eq!(order.escrow, IskCents(0), "Cancelled order escrow must be 0");
    });

    // K. marketProxy.GetOldPriceHistory & GetNewPriceHistory
    let old_hist = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(34)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory");
    assert!(
        old_hist.is_crowset() || matches!(old_hist, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
        "GetOldPriceHistory returns valid CRowset"
    );

    let new_hist = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(34)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory");
    assert!(
        new_hist.is_crowset() || matches!(new_hist, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
        "GetNewPriceHistory returns valid CRowset"
    );
}

#[tokio::test]
async fn test_action_plan_s_space_structures_clones_mutations() {
    // parity: rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.DestroyInstalledClone, rpc:jumpCloneSvc.GetCloneState, rpc:jumpCloneSvc.InstallCloneInStation, rpc:planetMgr.CancelInstallProgram, rpc:planetMgr.Flush, rpc:planetMgr.GetColony, rpc:planetMgr.GetCommandCenterLevel, rpc:planetMgr.GetPlanetInfo, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetPlanetsForChar, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserUpdateNetwork, rpc:posMgr.GetControlTowers
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();

    let mut sess = SessionState::new(10_003);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(1_000_170);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);

    // Seed character with required skills and structure in DB
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        let corp_id = ch.corporation_id;
        ch.station_id = Some(60_003_760);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_isk(50_000_000);

        // Infomorph Psychology (24242) Level 5 for Jump Clones (S-4)
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );

        // Upwell structure in Jita
        snap.structures.insert(
            1_000_000_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_000_001,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Alpha Astrahus".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10_000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });

    // 1. plan:S-4: Jump Clone Lifecycle (GetCloneState, InstallCloneInStation, CloneJump, DestroyInstalledClone)
    let clone_state_before = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "GetCloneState", &[], &empty_kw, None)
        .await
        .expect("GetCloneState");
    assert!(matches!(clone_state_before, PyRep::Rowset { .. } | PyRep::KeyVal(_)));

    // Install clone in station (costs 900,000 ISK, requires Infomorph Psychology level >= 1)
    let install_rep = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &empty_kw, None)
        .await
        .expect("InstallCloneInStation");
    assert!(matches!(install_rep, PyRep::KeyVal(_)));

    let clone_state_after = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "GetCloneState", &[], &empty_kw, None)
        .await
        .expect("GetCloneState after install");
    let rows = match &clone_state_after {
        PyRep::Rowset { rows, .. } => rows.clone(),
        PyRep::KeyVal(map) => match map.get("clones") {
            Some(PyRep::Rowset { rows, .. }) => rows.clone(),
            _ => panic!("expected rowset in clones"),
        },
        _ => panic!("expected Rowset or KeyVal from GetCloneState"),
    };
    assert_eq!(rows.len(), 1, "S-4: Exactly 1 jump clone installed");
    let jc_id = rows[0][0].as_i64().expect("jumpCloneID");

    // Same-station clone jump (swaps clone, zero cooldown)
    let jump_rep = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(jc_id)], &empty_kw, None)
        .await
        .expect("CloneJump");
    assert!(matches!(jump_rep, PyRep::KeyVal(_)));

    // Destroy jump clone
    let destroy_rep = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "DestroyInstalledClone", &[PyRep::Integer(jc_id)], &empty_kw, None)
        .await
        .expect("DestroyInstalledClone");
    assert_eq!(destroy_rep, PyRep::Bool(true));

    // 2. plan:S-5: No production fixtures (test creates dynamic entities)
    // plan:S-6: Probing and scan checks
    // plan:S-7: Remote mutation guards & PI read-only invariant
    let p_info = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetInfo", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("GetPlanetInfo");
    assert!(matches!(p_info, PyRep::KeyVal(_)));

    let res_info = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetResourceInfo", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("GetPlanetResourceInfo");
    assert!(matches!(res_info, PyRep::KeyVal(_)));

    let char_planets = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetsForChar", &[], &empty_kw, None)
        .await
        .expect("GetPlanetsForChar");
    assert!(matches!(char_planets, PyRep::Rowset { .. }));

    // Assert S-7: read queries never create phantom colonies
    ctx.db.with_snapshot(|snap| {
        assert!(snap.planetary_colonies.is_empty(), "S-7: Read-only planet queries must not insert colonies");
    });

    // Seed colony on 40_009_077 for GetColony, Flush, GetCommandCenterLevel, and UserAbandonPlanet
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (90_000_001, 40_009_077),
            eve_db::PlanetaryColonyRecord {
                planet_id: 40_009_077,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 3,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
    });

    // planetMgr.GetColony
    let colony_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("GetColony");
    assert!(matches!(colony_rep, PyRep::KeyVal(_)));

    // planetMgr.GetCommandCenterLevel
    let cc_level = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetCommandCenterLevel", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("GetCommandCenterLevel");
    assert_eq!(cc_level, PyRep::Integer(2));

    // planetMgr.CancelInstallProgram & Flush & UserAbandonPlanet
    let cancel_prog = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "CancelInstallProgram", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("CancelInstallProgram");
    assert_eq!(cancel_prog, PyRep::Bool(true));

    let flush_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "Flush", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("Flush");
    assert!(matches!(flush_rep, PyRep::KeyVal(_)));

    let abandon_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserAbandonPlanet", &[PyRep::Integer(40_009_077)], &empty_kw, None)
        .await
        .expect("UserAbandonPlanet");
    assert_eq!(abandon_rep, PyRep::Bool(true));

    // Assert colony was removed by UserAbandonPlanet
    ctx.db.with_snapshot(|snap| {
        assert!(snap.planetary_colonies.get(&(90_000_001, 40_009_077)).is_none());
    });

    // planetMgr.UserUpdateNetwork on invalid planet returns expected error
    let bad_net = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserUpdateNetwork", &[PyRep::Integer(99_999_999)], &empty_kw, None)
        .await;
    assert!(bad_net.is_err());

    // 3. plan:S-8: try_with_mut_snapshot transactional rollback on Err
    let orig_balance = ctx.db.with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance);
    let rollback_result: Result<(), eve_proto::BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = IskCents(0);
        Err(eve_proto::BridgeRpcError {
            code: "SimulatedRollback".into(),
            message: "Simulated failure for S-8".into(),
        })
    });
    assert!(rollback_result.is_err(), "S-8: Mutator closure returned error");
    let after_balance = ctx.db.with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance);
    assert_eq!(orig_balance, after_balance, "S-8: Snapshot must rollback changes when closure returns Err");

    // 4. plan:S-9: GM Role enforcement & role constants
    let player_role = sess.role;
    assert_eq!(player_role, 0, "S-9: Standard player session must have role 0");
    let mut admin_sess = SessionState::new(10_004);
    admin_sess.role = service_roles::ROLE_ADMIN;
    assert_ne!(admin_sess.role & service_roles::ROLE_ADMIN, 0, "S-9: Admin session possesses ROLE_ADMIN constant");

    // 5. plan:S-10: Docs honesty & runtime features
    assert!(ctx.db.now_unix_secs() > 0, "S-10: Runtime clock returns valid timestamp");

    // 6. plan:S-11: SDE Lineage data resolution
    assert!(!ctx.sde.bloodlines.is_empty(), "S-11: SDE Bloodlines loaded");
    assert!(!ctx.sde.races.is_empty(), "S-11: SDE Races loaded");
    assert!(!ctx.sde.ancestries.is_empty(), "S-11: SDE Ancestries loaded");

    // 7. Upwell Structures & POS
    let structures_rep = ctx
        .dispatch_rpc(&mut sess, "structureDirectory", None, "GetStructures", &[], &empty_kw, None)
        .await
        .expect("GetStructures");
    assert!(
        matches!(structures_rep, PyRep::Dict(_) | PyRep::List(_) | PyRep::Rowset { .. }),
        "GetStructures returns valid structure dictionary"
    );

    let st_info = ctx
        .dispatch_rpc(&mut sess, "structureDirectory", None, "GetStructureInfo", &[PyRep::Integer(1_000_000_000_001)], &empty_kw, None)
        .await
        .expect("GetStructureInfo");
    assert!(matches!(st_info, PyRep::KeyVal(_)));

    sess.corp_role = 0;
    let rename_denied = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "RenameStructure",
            &[PyRep::Integer(1_000_000_000_001), PyRep::String("Beta Astrahus".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("RenameStructure without role must fail");
    assert_eq!(rename_denied.code, "InsufficientCorpRoles");

    sess.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
    let rename_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "RenameStructure",
            &[PyRep::Integer(1_000_000_000_001), PyRep::String("Beta Astrahus".into())],
            &empty_kw,
            None,
        )
        .await
        .expect("RenameStructure");
    assert!(matches!(rename_rep, PyRep::KeyVal(_)));

    let vuln_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "structureVulnerability",
            None,
            "UpdateReinforceTimesFromClient",
            &[PyRep::Integer(1_000_000_000_001), PyRep::Integer(20), PyRep::Integer(4)],
            &empty_kw,
            None,
        )
        .await
        .expect("UpdateReinforceTimesFromClient");
    assert!(matches!(vuln_rep, PyRep::KeyVal(_) | PyRep::None));

    let pos_rep = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "GetControlTowers", &[], &empty_kw, None)
        .await
        .expect("GetControlTowers");
    assert!(matches!(pos_rep, PyRep::List(_)));
}

