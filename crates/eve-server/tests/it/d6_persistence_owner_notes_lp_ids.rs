//! SQLite restart persistence integration tests for `D-3` / `D-6` Batch 1:
//! - `feat:lifecycle.owner_notes-persistence`: Market order metadata (`issueDate`, `duration`,
//!   `range`, `accountID`) and scheduled expiry, corporation industry job delivery metadata, and
//!   Planetary Interaction colony networks (`pins`, `links`, `routes`, `extractor_programs`,
//!   processor `schematicID`) survive a full SQLite server restart.
//! - `feat:lifecycle.lp-durable`: Corporation LP wallets (`corp_lp_balances`) and character LP
//!   reward logs (`char_lp_logs`) survive a SQLite server restart, and failed LP store purchases
//!   leave LP, ISK, and inventory items unchanged.
//! - `feat:invariant.ids-monotonic-across-restart`: IDs allocated for alliances, corporations,
//!   notifications, in-game reports, and persisted entities are strictly greater after a restart
//!   than any ID allocated before the restart, never overwriting existing records.

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::ingamereport_ops;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const TRITANIUM: TypeId = 34;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const RIFTER_TYPE_ID: TypeId = 587;
const CALDARI_NAVY_CORP_ID: i64 = 1_000_035;
const FEDERATION_NAVY_CORP_ID: i64 = 1_000_036;
const CONCORD_CORP_ID: i64 = 1_000_125;

const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2534;
const GAS_EXTRACTOR_TYPE_ID: TypeId = 2848;
const GAS_BASIC_FACILITY_TYPE_ID: TypeId = 2492;
const GAS_LAUNCHPAD_TYPE_ID: TypeId = 2543;

fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
    let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
    let ctx = ServerContext::with_repository(Arc::new(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    call(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
}

async fn call_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    m: &str,
    args: &[PyRep],
    kw: &[(&str, PyRep)],
) -> Result<PyRep, BridgeRpcError> {
    let kwargs: HashMap<String, PyRep> = kw.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    ctx.dispatch_rpc(sess, svc, None, m, args, &kwargs, None).await
}

fn rowset_maps(rep: &PyRep) -> Vec<HashMap<String, PyRep>> {
    let PyRep::Rowset { columns, rows } = rep else { panic!("expected Rowset, got {rep:?}") };
    rows.iter().map(|r| columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
}

fn seed_corp_for_pilot(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, corp_id: i64) {
    let mut corp = CorporationRecord {
        corporation_id: corp_id,
        name: format!("Persistence Test Corp {corp_id}"),
        ticker: "PRST".into(),
        ceo_id: char_id,
        ..Default::default()
    };
    corp.ensure_wallet_divisions();
    for d in &mut corp.wallet_divisions {
        d.balance = IskCents::from_isk(50_000_000);
    }
    corp.master_wallet = IskCents::from_isk(50_000_000);
    corp.members.insert(char_id);
    corp.member_roles.insert(char_id, u64::MAX);
    snap.corporations.insert(corp_id, corp);
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.corporation_id = corp_id;
        ch.balance = IskCents::from_isk(100_000_000);
    }
}

fn cmd_pin(id: i64, tid: TypeId, lat: f64, lon: f64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(lat), PyRep::Real(lon)])
}
fn cmd_link(a: i64, b: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(0)])
}
fn cmd_upgrade_link(a: i64, b: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(lvl)])
}
fn cmd_upgrade_cc(id: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(id), PyRep::Integer(lvl)])
}
fn cmd_schematic(id: i64, sid: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(id), PyRep::Integer(sid)])
}
fn cmd_route(id: i64, path: &[i64], tid: TypeId, qty: i64) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(6),
        PyRep::Integer(id),
        PyRep::List(path.iter().map(|&p| PyRep::Integer(p)).collect()),
        PyRep::Integer(tid as i64),
        PyRep::Integer(qty),
    ])
}
fn cmd_extractor_program(id: i64, tid: TypeId, radius: f64, heads: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(radius), PyRep::Integer(heads)])
}

#[tokio::test]
async fn test_d3_owner_notes_persistence_market_industry_and_pi_across_sqlite_restart() {
    // parity: feat:lifecycle.owner_notes-persistence
    eve_testkit::attest(&["feat:lifecycle.owner_notes-persistence"]);

    let path = std::env::temp_dir().join(format!("evomu_d3_owner_notes_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let corp_id = 98_200_101;
    let base_time: u64 = 1_750_000_000;

    let (order_id, expected_issue_filetime, order_escrow_isk, corp_job_id, first_p0_tid) = {
        let ctx = open_ctx_at(&path, base_time);
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            seed_corp_for_pilot(snap, P1, corp_id);
            let sk = snap.skills.entry(P1).or_default();
            for (stid, lvl) in [(3443, 5), (16596, 5), (2505, 5), (2495, 4), (13279, 4), (2406, 3), (2403, 2)] {
                sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
            }
            snap.next_entity_id += 1;
            let bp_id = snap.next_entity_id;
            snap.items.insert(
                bp_id,
                InventoryItemRecord {
                    item_id: bp_id,
                    type_id: RIFTER_BP_TYPE_ID,
                    owner_id: corp_id,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let mat_id = snap.next_entity_id;
            snap.items.insert(
                mat_id,
                InventoryItemRecord {
                    item_id: mat_id,
                    type_id: TRITANIUM,
                    owner_id: corp_id,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 100_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let mut sess = login_select(&ctx, 7001, "demo", "demo-secret", P1).await;

        // 1. Place a corporate buy order with duration = 1 day, range = 5 jumps, accountKey = 1002 (Division 3)
        let buy_res = call(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM as i64),
                PyRep::Real(3.50),
                PyRep::Integer(1_000),
                PyRep::Integer(5),    // range = 5 jumps
                PyRep::Integer(10),   // minVolume = 10
                PyRep::Integer(1),    // duration = 1 day
                PyRep::Bool(true),    // useCorp = true
                PyRep::Integer(1002), // accountKey = 1002 (Division 3)
            ],
        )
        .await
        .expect("PlaceBuyOrder duration=1 range=5 accountKey=1002");
        let order_id = buy_res.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

        let corp_orders = rowset_maps(&call(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
        let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        let issue_ft = ord_row.get("issueDate").and_then(|v| v.as_i64()).unwrap();
        let escrow_isk = ord_row.get("escrow").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(ord_row.get("duration").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(ord_row.get("range").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(ord_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));

        // 2. Install a corporation manufacturing job (`useCorp: true`)
        let install_res = call_kw(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64), PyRep::Integer(1), PyRep::Integer(1)],
            &[("useCorp", PyRep::Bool(true))],
        )
        .await
        .expect("InstallJob corp manufacturing");
        let corp_job_id = install_res.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");

        // 3. Undock into Jita space, seed Gas Command Center in active ship cargo, and build a full PI network
        let ship_id = sess.ship_id.expect("active ship");
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = None;
                ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            }
            snap.next_entity_id += 1;
            let cc_item_id = snap.next_entity_id;
            snap.items.insert(
                cc_item_id,
                InventoryItemRecord {
                    item_id: cc_item_id,
                    type_id: GAS_COMMAND_CENTER_TYPE_ID,
                    owner_id: P1,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let res_info = call(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap();
        let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!("expected resources list") };
        let first_p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap() as TypeId;

        let pi_cmds = vec![
            cmd_pin(101, GAS_COMMAND_CENTER_TYPE_ID, 0.10, 0.10),
            cmd_upgrade_cc(101, 3),
            cmd_pin(102, GAS_EXTRACTOR_TYPE_ID, 0.11, 0.11),
            cmd_pin(103, GAS_BASIC_FACILITY_TYPE_ID, 0.12, 0.12),
            cmd_pin(104, GAS_LAUNCHPAD_TYPE_ID, 0.13, 0.13),
            cmd_link(101, 102),
            cmd_link(102, 103),
            cmd_link(103, 104),
            cmd_upgrade_link(102, 103, 1),
            cmd_schematic(103, 65),
            cmd_route(501, &[102, 103], first_p0_tid, 3_000),
            cmd_extractor_program(102, first_p0_tid, 0.02, 3),
        ];
        call(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(pi_cmds)])
            .await
            .expect("UserUpdateNetwork");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = Some(JITA_STATION_ID);
            }
        });
        (order_id, issue_ft, escrow_isk, corp_job_id, first_p0_tid)
    };

    // === RESTART SERVER FROM SQLITE ===
    {
        // Set clock to 12 hours after base_time: 1-day order is still open, corp job (3600s) is ready!
        let ctx = open_ctx_at(&path, base_time + 43_200);
        let mut sess = login_select(&ctx, 7002, "demo", "demo-secret", P1).await;

        // 1a. Verify market order kept issueDate, duration=1, range=5, accountKey=1002 after restart
        let corp_orders = rowset_maps(&call(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
        let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        assert_eq!(ord_row.get("issueDate").and_then(|v| v.as_i64()), Some(expected_issue_filetime));
        assert_eq!(ord_row.get("duration").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(ord_row.get("range").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(ord_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));

        // 1b. Advance clock past 1 day (base_time + 86_401) and verify order expires on schedule and refunds division 3 (accountKey 1002)
        let div3_before = ctx.db.with_snapshot(|s| {
            s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance
        });
        ctx.advance_secs(43_201);
        let hist_rows = rowset_maps(
            &call_kw(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)], &[("isCorp", PyRep::Bool(true))])
                .await
                .expect("GetMarketOrderHistory sweeps expired 1-day order"),
        );
        let exp_row = hist_rows.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        assert_eq!(exp_row.get("orderState").and_then(|v| v.as_i64()), Some(4));
        assert_eq!(exp_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));
        let div3_after = ctx.db.with_snapshot(|s| {
            s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance
        });
        assert_eq!(div3_after.0 - div3_before.0, (order_escrow_isk * 100.0).round() as i64);

        // 2. Complete the corp industry job after restart and verify it delivers the manufactured Rifter to the corporation (`owner_id == corp_id`)
        let complete_res = call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await.unwrap();
        let delivered_item_id = complete_res.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
        let delivered_item = ctx.db.with_snapshot(|s| s.items.get(&delivered_item_id).cloned()).unwrap();
        assert_eq!(delivered_item.type_id, RIFTER_TYPE_ID);
        assert_eq!(delivered_item.owner_id, corp_id);

        // 3. Verify PI colony pins, links, routes, schematic, and extractor program are intact after restart
        let net_after = call(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)])
            .await
            .expect("GetFullNetworkForOwner after restart");
        assert_eq!(net_after.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));
        let PyRep::List(pins_after) = net_after.dict_get("pins").unwrap() else { panic!("expected pins list") };
        assert_eq!(pins_after.len(), 4);
        let proc_pin = pins_after.iter().find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(GAS_BASIC_FACILITY_TYPE_ID as i64)).unwrap();
        assert_eq!(proc_pin.dict_get("schematicID").and_then(|v| v.as_i64()), Some(65));

        let PyRep::List(links_after) = net_after.dict_get("links").unwrap() else { panic!("expected links list") };
        assert_eq!(links_after.len(), 3);
        assert!(links_after.iter().any(|l| l.dict_get("level").and_then(|v| v.as_i64()) == Some(1)));

        let PyRep::List(routes_after) = net_after.dict_get("routes").unwrap() else { panic!("expected routes list") };
        assert_eq!(routes_after.len(), 1);
        assert_eq!(routes_after[0].dict_get("routeID").and_then(|v| v.as_i64()), Some(501));
        assert_eq!(routes_after[0].dict_get("commodityTypeID").and_then(|v| v.as_i64()), Some(first_p0_tid as i64));
        assert_eq!(routes_after[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(3_000));

        let ext_pin_id = pins_after
            .iter()
            .find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(GAS_EXTRACTOR_TYPE_ID as i64))
            .and_then(|p| p.dict_get("pinID"))
            .and_then(|v| v.as_i64())
            .expect("extractor pinID");
        let ext_pin = call(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)])
            .await
            .expect("GetPin Extractor after restart");
        assert_eq!(ext_pin.dict_get("headCount").and_then(|v| v.as_i64()), Some(3));
        assert_eq!(ext_pin.dict_get("resourceTypeID").and_then(|v| v.as_i64()), Some(first_p0_tid as i64));
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_d6_lp_durable_and_atomic_failed_purchase_across_sqlite_restart() {
    // parity: feat:lifecycle.lp-durable
    eve_testkit::attest(&["feat:lifecycle.lp-durable"]);

    let path = std::env::temp_dir().join(format!("evomu_d6_lp_durable_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let corp_id = 98_200_202;

    let (corp_lp_before, logs_before) = {
        let ctx = open_ctx_at(&path, 1_750_100_000);
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            seed_corp_for_pilot(snap, P1, corp_id);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 15_000);
                ch.loyalty_points.insert(CONCORD_CORP_ID, 5_000);
            }
            snap.items.retain(|_, it| !(it.owner_id == P1 && it.location_id == JITA_STATION_ID && it.type_id == TRITANIUM));
            snap.next_entity_id += 1;
            let tid_item = snap.next_entity_id;
            snap.items.insert(
                tid_item,
                InventoryItemRecord {
                    item_id: tid_item,
                    type_id: TRITANIUM,
                    owner_id: P1,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 50,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let mut sess = login_select(&ctx, 7101, "demo", "demo-secret", P1).await;

        call(
            &ctx,
            &mut sess,
            "LPSvc",
            "TransferLPFromMyWalletToOtherCorp",
            &[PyRep::Integer(corp_id), PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(6_000)],
        )
        .await
        .expect("TransferLPFromMyWalletToOtherCorp");

        call(&ctx, &mut sess, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(FEDERATION_NAVY_CORP_ID), PyRep::Integer(1_000)])
            .await
            .expect("ExchangeConcordLP");

        let corp_lp = call(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
        let logs = call(&ctx, &mut sess, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
        (corp_lp, logs)
    };

    // === RESTART SERVER FROM SQLITE ===
    {
        let ctx = open_ctx_at(&path, 1_750_100_060);
        let mut sess = login_select(&ctx, 7102, "demo", "demo-secret", P1).await;

        let corp_lp_after = call(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
        let logs_after = call(&ctx, &mut sess, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();

        assert_eq!(corp_lp_after, corp_lp_before, "Corporation LP wallet balances must survive restart");
        assert_eq!(logs_after, logs_before, "Character LP reward logs must survive restart");
        let PyRep::List(log_entries) = &logs_after else { panic!("expected List of LP logs") };
        assert_eq!(log_entries.len(), 2);

        let snap_state = |ctx: &ServerContext| {
            ctx.db.with_snapshot(|s| {
                let ch = &s.characters[&P1];
                let trit_qty: i32 = s
                    .items
                    .values()
                    .filter(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.type_id == TRITANIUM)
                    .map(|i| i.quantity)
                    .sum();
                let bcs_qty: i32 = s
                    .items
                    .values()
                    .filter(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.type_id == 15810)
                    .map(|i| i.quantity)
                    .sum();
                (ch.loyalty_points.clone(), s.corp_lp_balances.clone(), ch.balance, trit_qty, bcs_qty)
            })
        };
        let before_fail = snap_state(&ctx);

        // 1. Fail on missing required trade-in items (Offer 1004 requires 100x Tritanium, pilot has 50x)
        let err_items = call(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1004), PyRep::Integer(1)],
        )
        .await
        .expect_err("Offer 1004 must fail when required trade-in items are insufficient");
        assert_eq!(err_items.code, "MissingRequiredItems");
        assert_eq!(snap_state(&ctx), before_fail);

        // 2. Fail on insufficient character LP (Offer 1003 costs 10,000 LP, pilot has 9,000 LP)
        let err_lp = call(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1003), PyRep::Integer(1)],
        )
        .await
        .expect_err("Offer 1003 must fail when character LP is insufficient");
        assert_eq!(err_lp.code, "InsufficientLoyaltyPoints");
        assert_eq!(snap_state(&ctx), before_fail);

        // 3. Fail on insufficient corporation LP (Offer 1003 costs 10,000 LP, corp has 6,000 LP)
        let err_corp_lp = call(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1003), PyRep::Integer(1)],
        )
        .await
        .expect_err("Offer 1003 for corp must fail when corp LP is insufficient");
        assert_eq!(err_corp_lp.code, "InsufficientLoyaltyPoints");
        assert_eq!(snap_state(&ctx), before_fail);
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_d6_ids_monotonic_across_sqlite_restart_and_never_overwrite_alliances() {
    // parity: feat:invariant.ids-monotonic-across-restart
    eve_testkit::attest(&["feat:invariant.ids-monotonic-across-restart"]);

    let path = std::env::temp_dir().join(format!("evomu_d6_monotonic_ids_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());

    let (corp_id_1, alliance_id_1, notif_id_1, report_id_1, entity_id_1) = {
        let ctx = open_ctx_at(&path, 1_750_200_000);
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            for cid in [P1, P2] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.balance = IskCents::from_isk(5_000_000_000);
                }
                snap.skills.entry(cid).or_default().insert(
                    3363,
                    TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
                );
            }
        });

        let mut sess1 = login_select(&ctx, 7201, "demo", "demo-secret", P1).await;

        let corp_rep_1 = call(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("First Horizon Industrials".into()), PyRep::String("FHZ1".into()), PyRep::Real(0.05)],
        )
        .await
        .expect("AddCorporation 1");
        let corp_id_1 = corp_rep_1.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id_1) {
                c.ensure_wallet_divisions();
                c.wallet_divisions[0].balance = IskCents::from_isk(2_000_000_000);
                c.master_wallet = c.wallet_divisions[0].balance;
            }
        });

        let al_rep_1 = call(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "CreateAlliance",
            &[
                PyRep::String("First Horizon Alliance".into()),
                PyRep::String("FHAL1".into()),
                PyRep::String("Pre-restart alliance".into()),
                PyRep::String("https://fhal1.example".into()),
            ],
        )
        .await
        .expect("CreateAlliance 1");
        let alliance_id_1 = al_rep_1.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();

        let notif_id_1 = ctx.create_character_notification(1_000_125, P1, 10, 1, "Pre-restart notification").notification_id;

        call(
            &ctx,
            &mut sess1,
            "ingamereport",
            "report_macro_use",
            &[PyRep::Integer(P2), PyRep::String("botting".into()), PyRep::String("pre-restart report".into())],
        )
        .await
        .expect("report_macro_use");
        let reports_1 = ingamereport_ops::get_persisted_reports(&ctx);
        assert_eq!(reports_1.len(), 1);
        let report_id_1 = reports_1[0].report_id;

        let fleet_rep_1 = call(&ctx, &mut sess1, "fleetSvc", "InitFleet", &[]).await.unwrap();
        let entity_id_1 = fleet_rep_1.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

        (corp_id_1, alliance_id_1, notif_id_1, report_id_1, entity_id_1)
    };

    // === RESTART SERVER FROM SQLITE ===
    {
        let ctx = open_ctx_at(&path, 1_750_200_100);
        let mut sess2 = login_select(&ctx, 7202, "other", "other-secret", P2).await;

        let corp_rep_2 = call(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("Second Horizon Logistics".into()), PyRep::String("SHZ2".into()), PyRep::Real(0.07)],
        )
        .await
        .expect("AddCorporation 2 after restart");
        let corp_id_2 = corp_rep_2.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id_2) {
                c.ensure_wallet_divisions();
                c.wallet_divisions[0].balance = IskCents::from_isk(2_000_000_000);
                c.master_wallet = c.wallet_divisions[0].balance;
            }
        });

        let al_rep_2 = call(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "CreateAlliance",
            &[
                PyRep::String("Second Horizon Alliance".into()),
                PyRep::String("SHAL2".into()),
                PyRep::String("Post-restart alliance".into()),
                PyRep::String("https://shal2.example".into()),
            ],
        )
        .await
        .expect("CreateAlliance 2 after restart");
        let alliance_id_2 = al_rep_2.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();

        let notif_id_2 = ctx.create_character_notification(1_000_125, P2, 10, 1, "Post-restart notification").notification_id;

        call(
            &ctx,
            &mut sess2,
            "ingamereport",
            "report_exploit_abuse",
            &[PyRep::Integer(P1), PyRep::String("exploit".into()), PyRep::String("post-restart report".into())],
        )
        .await
        .expect("report_exploit_abuse after restart");
        let reports_all = ingamereport_ops::get_persisted_reports(&ctx);
        assert_eq!(reports_all.len(), 2);
        let report_id_2 = reports_all.iter().find(|r| r.category == "exploit_abuse").unwrap().report_id;

        let fleet_rep_2 = call(&ctx, &mut sess2, "fleetSvc", "InitFleet", &[]).await.unwrap();
        let entity_id_2 = fleet_rep_2.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

        assert!(corp_id_2 > corp_id_1, "corp_id_2 ({corp_id_2}) > corp_id_1 ({corp_id_1})");
        assert!(alliance_id_2 > alliance_id_1, "alliance_id_2 ({alliance_id_2}) > alliance_id_1 ({alliance_id_1})");
        assert!(notif_id_2 > notif_id_1, "notif_id_2 ({notif_id_2}) > notif_id_1 ({notif_id_1})");
        assert!(report_id_2 > report_id_1, "report_id_2 ({report_id_2}) > report_id_1 ({report_id_1})");
        assert!(entity_id_2 > entity_id_1, "entity_id_2 ({entity_id_2}) > entity_id_1 ({entity_id_1})");

        ctx.db.with_snapshot(|snap| {
            let a1 = snap.alliances.get(&alliance_id_1).expect("Pre-restart alliance must still exist intact");
            let a2 = snap.alliances.get(&alliance_id_2).expect("Post-restart alliance must exist");
            assert_eq!(a1.name, "First Horizon Alliance");
            assert_eq!(a1.short_name, "FHAL1");
            assert_eq!(a1.executor_corp_id, corp_id_1);
            assert_eq!(a2.name, "Second Horizon Alliance");
            assert_eq!(a2.short_name, "SHAL2");
            assert_eq!(a2.executor_corp_id, corp_id_2);
        });
    }

    let _ = std::fs::remove_file(&path);
}
