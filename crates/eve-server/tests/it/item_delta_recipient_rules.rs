//! Dispatcher-level integration acceptance tests for Wave F1 `#931` Track 5C Step 3a (`Section 4F`).
//!
//! Covers:
//! 1. Foreign ship bay operations (`SplitStack`, `MultiMerge`, `StackAll`, `MultiAdd`, `Add`) via
//!    `dispatch_rpc`: both participants receive each expected `OnItemChange` **exactly once**.
//! 2. Ownerless item in a floating space container: partial take delivers source-stack update once.
//! 3. Closed corporation office tombstone (`locationID == 6`) reaches former member once via both
//!    RPC (`corpRegistry.KickOutMember`) and downtime (`execute_pending_auto_kicks_on_downtime`).
//! 4. Station non-leak: acting pilot's RPC changing another character's item and a non-member corp's
//!    item directly in a station leaks zero `OnItemChange` notifications to the acting pilot.
//! 5. Outside-RPC ownerless mutation and same-commit ownerless space container + contents creation
//!    emit nothing to the acting pilot.
//! 6. Own item moved into own corporation's hangar emits **exactly one** `OnItemChange` per member.
//! 7. Quantum core installation suppresses the post-consumption hand-written `"moved"` event.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_testkit::client_harness::{ClientErrorResponse, ClientHarness};
use eve_server::router::cache_notifications::PendingMachoNotification;
use eve_server::router::test_context::run_staged_dispatch_commit;
use eve_server::router::ServerContext;
use eve_server::services::fleet_social::execute_pending_auto_kicks_on_downtime;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

use crate::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: std::sync::OnceLock<Arc<SdeRegistry>> = std::sync::OnceLock::new();
    Arc::clone(
        SDE.get_or_init(|| {
            Arc::new(SdeRegistry::open(&SdeRegistry::default_db_path()).expect("open SDE"))
        }),
    )
}

fn paused_ctx_with_sde(sde: Arc<SdeRegistry>) -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        sde,
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn login_select(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    let sel = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
    )
    .await
    .expect("SelectCharacterID");
    assert!(!matches!(sel, PyRep::None) || sess.character_id == Some(char_id));
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound: Option<u64>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, ClientErrorResponse> {
    ClientHarness::new(common::SessionBorrowWireTransport {
        ctx,
        session: sess,
        preserve_char_notifications: true,
    })
    .call(service, bound, method, args, kwargs)
    .await
}

async fn bind_inv(
    ctx: &ServerContext,
    sess: &mut SessionState,
    bind_loc_id: i64,
    group_arg: i64,
    target_inv_id: i64,
) -> u64 {
    let kw = HashMap::new();
    let bind_rep = rpc(
        ctx,
        sess,
        "invbroker",
        None,
        "MachoBindObject",
        &[
            PyRep::Tuple(vec![PyRep::Integer(bind_loc_id), PyRep::Integer(group_arg)]),
            PyRep::None,
        ],
        &kw,
    )
    .await
    .expect("invbroker.MachoBindObject");
    let mgr_oid = eve_testkit::client_harness::extract_bound_id_from_wire_rep(&bind_rep)
        .unwrap_or_else(|| panic!("expected bound invmgr ID, got {bind_rep:?}"));
    let inv_rep = rpc(
        ctx,
        sess,
        "invbroker",
        Some(mgr_oid),
        "GetInventoryFromId",
        &[PyRep::Integer(target_inv_id), PyRep::Integer(0)],
        &kw,
    )
    .await
    .expect("invbroker.GetInventoryFromId");
    inv_rep
        .as_i64()
        .map(|id| id as u64)
        .or_else(|| eve_testkit::client_harness::extract_bound_id_from_wire_rep(&inv_rep))
        .unwrap_or_else(|| panic!("expected bound inv ID, got {inv_rep:?}"))
}

fn dbrow_col_i64(row: &PyRep, col: &str) -> Option<i64> {
    let PyRep::DBRow { columns, .. } = row else {
        return None;
    };
    columns
        .iter()
        .find(|(name, _)| name == col)
        .and_then(|(_, val)| val.as_i64())
}

fn count_item_notifs(
    notifs: &[PendingMachoNotification],
    item_id: i64,
    qty: Option<i64>,
    loc: Option<i64>,
    flag: Option<i64>,
) -> usize {
    notifs
        .iter()
        .filter(|n| {
            n.event == "OnItemChange"
                && !n.args.is_empty()
                && dbrow_col_i64(&n.args[0], "itemID") == Some(item_id)
                && qty.is_none_or(|q| dbrow_col_i64(&n.args[0], "quantity") == Some(q))
                && loc.is_none_or(|l| dbrow_col_i64(&n.args[0], "locationID") == Some(l))
                && flag.is_none_or(|f| dbrow_col_i64(&n.args[0], "flagID") == Some(f))
        })
        .count()
}

fn insert_item_rec(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn grant_next_item(
    ctx: &ServerContext,
    owner_id: i64,
    location_id: i64,
    type_id: u32,
    flag: InventoryFlag,
    quantity: i32,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1 && !ctx.sde.types.get(&type_id).is_some_and(|t| t.category_id == 4),
                version: 1,
                ..Default::default()
            },
        );
        id
    })
}

fn build_carrier_sde(carrier_tid: u32) -> Arc<SdeRegistry> {
    let mut sde_mut = (*shared_sde()).clone();
    let mut carrier = sde_mut.types[&587].clone();
    carrier.type_id = carrier_tid;
    carrier.group_id = 30;
    carrier.category_id = 6;
    carrier.name = format!("Foreign Bay Carrier {carrier_tid}");
    carrier.capacity = 1_000.0;
    carrier.default_attributes.insert(908, 10_000_000.0);
    carrier.default_attributes.insert(912, 10_000_000.0);
    sde_mut.types.insert(carrier_tid, carrier);
    Arc::new(sde_mut)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_foreign_ship_bay_split_multimerge_stackall_and_multiadd_notify_both_participants() {
    let carrier_tid = 999_931_u32;
    let ctx = paused_ctx_with_sde(build_carrier_sde(carrier_tid));
    let mut owner_sess = login_select(&ctx, 73_101, "demo", "demo-secret", PILOT_1).await;
    let mut mate_sess = login_select(&ctx, 73_102, "other", "other-secret", PILOT_2).await;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].corporation_id);
    let owner_ship = 1_000_931_001_i64;
    let mate_ship = mate_sess.ship_id.expect("mate active ship");
    let fh_trit_a = 1_000_931_010_i64;
    let mate_fh_trit = 1_000_931_011_i64;
    let mate_fh_partial = 1_000_931_012_i64;
    let fh_pyerite = 1_000_931_013_i64;
    let mate_hangar_merge = 1_000_931_014_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ship) in [(PILOT_1, owner_ship), (PILOT_2, mate_ship)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
                ch.solar_system_id = JITA_SYS;
                ch.active_ship_id = ship;
                ch.fleet_id = Some(777_931);
            }
        }
        snap.fleets.insert(
            777_931,
            FleetRecord {
                fleet_id: 777_931,
                boss_id: PILOT_1,
                members: vec![PILOT_1, PILOT_2],
                ..Default::default()
            },
        );
        snap.items.insert(
            owner_ship,
            InventoryItemRecord {
                item_id: owner_ship,
                type_id: carrier_tid,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    owner_sess.station_id = Some(JITA_STATION);
    owner_sess.ship_id = Some(owner_ship);
    owner_sess.fleet_id = Some(777_931);
    mate_sess.station_id = Some(JITA_STATION);
    mate_sess.corporation_id = Some(corp_id);
    mate_sess.fleet_id = Some(777_931);

    let cfg_kw = HashMap::new();
    let cfg_rep = rpc(
        &ctx,
        &mut owner_sess,
        "ship",
        None,
        "ConfigureShip",
        &[
            PyRep::Integer(owner_ship),
            PyRep::Dict(vec![
                (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(true)),
                (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(true)),
            ]),
        ],
        &cfg_kw,
    )
    .await
    .expect("ConfigureShip fleet access 1a");
    assert_eq!(cfg_rep.dict_get("FleetHangar_AllowFleetAccess"), Some(&PyRep::Bool(true)));

    insert_item_rec(&ctx, fh_trit_a, 34, PILOT_1, owner_ship, InventoryFlag::FleetHangar, 100, false);
    insert_item_rec(&ctx, mate_fh_trit, 34, PILOT_2, owner_ship, InventoryFlag::FleetHangar, 25, false);
    insert_item_rec(&ctx, mate_fh_partial, 34, PILOT_2, owner_ship, InventoryFlag::FleetHangar, 10, false);
    insert_item_rec(&ctx, fh_pyerite, 35, PILOT_1, owner_ship, InventoryFlag::FleetHangar, 10, false);
    insert_item_rec(&ctx, mate_hangar_merge, 35, PILOT_2, JITA_STATION, InventoryFlag::Hangar, 15, false);

    let bound_owner_for_mate = bind_inv(&ctx, &mut mate_sess, JITA_STATION, 15, owner_ship).await;
    let kw = HashMap::new();

    // 1. SplitStack via dispatcher notifies both participants exactly once each.
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    let split_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "SplitStack",
        &[PyRep::Integer(owner_ship), PyRep::Integer(fh_trit_a), PyRep::Integer(40)],
        &kw,
    )
    .await
    .expect("dispatcher SplitStack");
    let new_split_id = split_rep
        .dict_get("newItemID")
        .and_then(PyRep::as_i64)
        .expect("newItemID");
    let owner_split = ctx.drain_notifications_for_char(PILOT_1);
    let mate_split = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_split, fh_trit_a, Some(60), Some(owner_ship), None), 1, "{owner_split:?}");
    assert_eq!(count_item_notifs(&mate_split, fh_trit_a, Some(60), Some(owner_ship), None), 1, "{mate_split:?}");
    assert_eq!(count_item_notifs(&owner_split, new_split_id, Some(40), Some(owner_ship), None), 1, "{owner_split:?}");
    assert_eq!(count_item_notifs(&mate_split, new_split_id, Some(40), Some(owner_ship), None), 1, "{mate_split:?}");

    // 2. MultiMerge via dispatcher notifies both participants exactly once per stack update.
    let merge_ops = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(mate_fh_trit), PyRep::Integer(fh_trit_a), PyRep::None]),
        PyRep::Tuple(vec![PyRep::Integer(mate_fh_partial), PyRep::Integer(new_split_id), PyRep::Integer(4)]),
    ]);
    let merge_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "MultiMerge",
        &[merge_ops],
        &kw,
    )
    .await
    .expect("dispatcher MultiMerge");
    assert!(matches!(merge_rep, PyRep::None | PyRep::Dict(_) | PyRep::KeyVal(_)));
    let owner_merge = ctx.drain_notifications_for_char(PILOT_1);
    let mate_merge = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_merge, mate_fh_trit, None, Some(6), None), 1, "{owner_merge:?}");
    assert_eq!(count_item_notifs(&mate_merge, mate_fh_trit, None, Some(6), None), 1, "{mate_merge:?}");
    assert_eq!(count_item_notifs(&owner_merge, fh_trit_a, Some(85), Some(owner_ship), None), 1, "{owner_merge:?}");
    assert_eq!(count_item_notifs(&mate_merge, fh_trit_a, Some(85), Some(owner_ship), None), 1, "{mate_merge:?}");
    assert_eq!(count_item_notifs(&owner_merge, mate_fh_partial, Some(6), Some(owner_ship), None), 1, "{owner_merge:?}");
    assert_eq!(count_item_notifs(&mate_merge, mate_fh_partial, Some(6), Some(owner_ship), None), 1, "{mate_merge:?}");
    assert_eq!(count_item_notifs(&owner_merge, new_split_id, Some(44), Some(owner_ship), None), 1, "{owner_merge:?}");
    assert_eq!(count_item_notifs(&mate_merge, new_split_id, Some(44), Some(owner_ship), None), 1, "{mate_merge:?}");

    // 3. StackAll via dispatcher notifies both participants exactly once each.
    let stack_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "StackAll",
        &[PyRep::Integer(155)],
        &kw,
    )
    .await
    .expect("dispatcher StackAll");
    assert_eq!(stack_rep.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
    let (survivor_id, deleted_id) = if fh_trit_a < new_split_id {
        (fh_trit_a, new_split_id)
    } else {
        (new_split_id, fh_trit_a)
    };
    let owner_stack = ctx.drain_notifications_for_char(PILOT_1);
    let mate_stack = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_stack, survivor_id, Some(129), Some(owner_ship), None), 1, "{owner_stack:?}");
    assert_eq!(count_item_notifs(&mate_stack, survivor_id, Some(129), Some(owner_ship), None), 1, "{mate_stack:?}");
    assert_eq!(count_item_notifs(&owner_stack, deleted_id, None, Some(6), None), 1, "{owner_stack:?}");
    assert_eq!(count_item_notifs(&mate_stack, deleted_id, None, Some(6), None), 1, "{mate_stack:?}");

    // 4. MultiAdd whole transfer into foreign Fleet Hangar notifies both participants exactly once each.
    let madd_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(mate_hangar_merge)]),
            PyRep::Integer(owner_ship),
            PyRep::Integer(155),
        ],
        &kw,
    )
    .await
    .expect("dispatcher MultiAdd");
    assert!(!matches!(madd_rep, PyRep::Dict(ref d) if d.is_empty()) || madd_rep == PyRep::None || matches!(madd_rep, PyRep::List(_) | PyRep::KeyVal(_) | PyRep::Dict(_)));
    let owner_madd = ctx.drain_notifications_for_char(PILOT_1);
    let mate_madd = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_madd, mate_hangar_merge, Some(15), Some(owner_ship), Some(155)), 1, "{owner_madd:?}");
    assert_eq!(count_item_notifs(&mate_madd, mate_hangar_merge, Some(15), Some(owner_ship), Some(155)), 1, "{mate_madd:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_foreign_bay_add_split_merge_and_intrabay_notify_both_participants() {
    let carrier_tid = 999_932_u32;
    let ctx = paused_ctx_with_sde(build_carrier_sde(carrier_tid));
    let mut owner_sess = login_select(&ctx, 73_201, "demo", "demo-secret", PILOT_1).await;
    let mut mate_sess = login_select(&ctx, 73_202, "other", "other-secret", PILOT_2).await;

    let owner_ship = 1_931_000_001_i64;
    let mate_ship = mate_sess.ship_id.expect("mate active ship");
    let mate_hangar_trit = 1_931_000_010_i64;
    let bay_ship = 1_931_000_011_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ship) in [(PILOT_1, owner_ship), (PILOT_2, mate_ship)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
                ch.solar_system_id = JITA_SYS;
                ch.active_ship_id = ship;
                ch.fleet_id = Some(777_932);
            }
        }
        snap.fleets.insert(
            777_932,
            FleetRecord {
                fleet_id: 777_932,
                boss_id: PILOT_1,
                members: vec![PILOT_1, PILOT_2],
                ..Default::default()
            },
        );
    });
    owner_sess.station_id = Some(JITA_STATION);
    owner_sess.ship_id = Some(owner_ship);
    owner_sess.fleet_id = Some(777_932);
    mate_sess.station_id = Some(JITA_STATION);
    mate_sess.fleet_id = Some(777_932);

    insert_item_rec(&ctx, owner_ship, carrier_tid, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    let cfg_rep = rpc(
        &ctx,
        &mut owner_sess,
        "ship",
        None,
        "ConfigureShip",
        &[
            PyRep::Integer(owner_ship),
            PyRep::Dict(vec![
                (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(true)),
                (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(true)),
            ]),
        ],
        &HashMap::new(),
    )
    .await
    .expect("ConfigureShip fleet access 1b");
    assert_eq!(cfg_rep.dict_get("FleetHangar_AllowFleetAccess"), Some(&PyRep::Bool(true)));
    insert_item_rec(&ctx, mate_hangar_trit, 34, PILOT_2, JITA_STATION, InventoryFlag::Hangar, 30, false);
    insert_item_rec(&ctx, bay_ship, 587, PILOT_1, owner_ship, InventoryFlag::FleetHangar, 1, true);

    let bound_owner_for_mate = bind_inv(&ctx, &mut mate_sess, JITA_STATION, 15, owner_ship).await;
    let bound_station_for_mate = bind_inv(&ctx, &mut mate_sess, JITA_STATION, 15, JITA_STATION).await;

    // 1. Splitting Add into foreign FleetHangar notifies BOTH mate and owner exactly once.
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    let mut split_in_kw = HashMap::new();
    split_in_kw.insert("qty".into(), PyRep::Integer(10));
    let split_in_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "Add",
        &[
            PyRep::Integer(mate_hangar_trit),
            PyRep::Integer(owner_ship),
            PyRep::Integer(InventoryFlag::FleetHangar as i64),
        ],
        &split_in_kw,
    )
    .await
    .expect("dispatcher split Add into FleetHangar");
    let fh_split_id = split_in_rep
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("split itemID");
    let owner_split_in = ctx.drain_notifications_for_char(PILOT_1);
    let mate_split_in = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_split_in, fh_split_id, Some(10), Some(owner_ship), Some(155)), 1, "{owner_split_in:?}");
    assert_eq!(count_item_notifs(&mate_split_in, fh_split_id, Some(10), Some(owner_ship), Some(155)), 1, "{mate_split_in:?}");
    assert_eq!(count_item_notifs(&mate_split_in, mate_hangar_trit, Some(20), Some(JITA_STATION), Some(4)), 1, "{mate_split_in:?}");

    // 2. Merging Add into foreign FleetHangar stack notifies BOTH mate and owner exactly once.
    let merge_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "Add",
        &[
            PyRep::Integer(mate_hangar_trit),
            PyRep::Integer(owner_ship),
            PyRep::Integer(InventoryFlag::FleetHangar as i64),
        ],
        &HashMap::new(),
    )
    .await
    .expect("dispatcher merge Add into FleetHangar");
    assert_eq!(merge_rep.dict_get("itemID").and_then(PyRep::as_i64), Some(fh_split_id));
    let owner_merge_in = ctx.drain_notifications_for_char(PILOT_1);
    let mate_merge_in = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_merge_in, fh_split_id, Some(30), Some(owner_ship), Some(155)), 1, "{owner_merge_in:?}");
    assert_eq!(count_item_notifs(&mate_merge_in, fh_split_id, Some(30), Some(owner_ship), Some(155)), 1, "{mate_merge_in:?}");
    assert_eq!(count_item_notifs(&mate_merge_in, mate_hangar_trit, None, Some(6), None), 1, "{mate_merge_in:?}");

    // 3. Splitting Add OUT of foreign FleetHangar notifies BOTH mate and owner on remainder exactly once.
    let mut split_out_kw = HashMap::new();
    split_out_kw.insert("qty".into(), PyRep::Integer(5));
    let split_out_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_station_for_mate),
        "Add",
        &[
            PyRep::Integer(fh_split_id),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &split_out_kw,
    )
    .await
    .expect("dispatcher split Add out of FleetHangar");
    let out_id = split_out_rep
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("out split itemID");
    let owner_split_out = ctx.drain_notifications_for_char(PILOT_1);
    let mate_split_out = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_split_out, fh_split_id, Some(25), Some(owner_ship), Some(155)), 1, "{owner_split_out:?}");
    assert_eq!(count_item_notifs(&mate_split_out, fh_split_id, Some(25), Some(owner_ship), Some(155)), 1, "{mate_split_out:?}");
    assert_eq!(count_item_notifs(&mate_split_out, out_id, Some(5), Some(JITA_STATION), Some(4)), 1, "{mate_split_out:?}");

    // 4. Intra-bay Add moving owner's ship FleetHangar -> ShipHangar notifies BOTH once.
    let intra_rep = rpc(
        &ctx,
        &mut mate_sess,
        "invbroker",
        Some(bound_owner_for_mate),
        "Add",
        &[
            PyRep::Integer(bay_ship),
            PyRep::Integer(owner_ship),
            PyRep::Integer(InventoryFlag::ShipHangar as i64),
        ],
        &HashMap::new(),
    )
    .await
    .expect("dispatcher intra-bay Add");
    assert_eq!(intra_rep.dict_get("flagID").and_then(PyRep::as_i64), Some(90));
    let owner_intra = ctx.drain_notifications_for_char(PILOT_1);
    let mate_intra = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(count_item_notifs(&owner_intra, bay_ship, None, Some(owner_ship), Some(90)), 1, "{owner_intra:?}");
    assert_eq!(count_item_notifs(&mate_intra, bay_ship, None, Some(owner_ship), Some(90)), 1, "{mate_intra:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_ownerless_space_container_partial_take_notifies_acting_pilot_once() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let mut sess = login_select(&ctx, 73_301, "demo", "demo-secret", PILOT_1).await;
    let kw = HashMap::new();
    let undock_rep = rpc(&ctx, &mut sess, "structureDocking", None, "Undock", &[], &kw)
        .await
        .expect("Undock");
    assert!(!format!("{undock_rep:?}").is_empty());
    let sys_id = sess.solar_system_id.expect("solar system");
    let ship_id = sess.ship_id.expect("ship");

    let jetcan_id = 1_932_000_001_i64;
    let stack_id = 1_932_000_002_i64;
    insert_item_rec(&ctx, jetcan_id, 23, 0, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
    insert_item_rec(&ctx, stack_id, 34, 0, jetcan_id, InventoryFlag::Cargo, 50, false);

    let bound_ship = bind_inv(&ctx, &mut sess, i64::from(sys_id), 5, ship_id).await;
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    let mut add_kw = HashMap::new();
    add_kw.insert("qty".into(), PyRep::Integer(15));
    let add_rep = rpc(
        &ctx,
        &mut sess,
        "invbroker",
        Some(bound_ship),
        "Add",
        &[
            PyRep::Integer(stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &add_kw,
    )
    .await
    .expect("dispatcher Add partial split from ownerless container");
    let loot_new_id = add_rep
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("looted itemID");

    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(
        count_item_notifs(&notifs, stack_id, Some(35), Some(jetcan_id), Some(InventoryFlag::Cargo as i64)),
        1,
        "pilot must receive source-stack update from ownerless space container exactly once: {notifs:?}"
    );
    assert_eq!(
        count_item_notifs(&notifs, loot_new_id, Some(15), Some(ship_id), Some(InventoryFlag::Cargo as i64)),
        1,
        "pilot must receive newly created stack in ship cargo once: {notifs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_corp_closed_office_tombstone_reaches_former_member_once_via_rpc_and_downtime() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let mut sess1 = login_select(&ctx, 73_401, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = login_select(&ctx, 73_402, "other", "other-secret", PILOT_2).await;
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [PILOT_1, PILOT_2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_isk(500_000_000);
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
                ch.solar_system_id = JITA_SYS;
            }
        }
    });
    sess1.station_id = Some(JITA_STATION);
    sess2.station_id = Some(JITA_STATION);

    let kw = HashMap::new();
    // 1. RPC path: solo CEO rents office, then self-quits via corpRegistry.KickOutMember -> corp closes.
    let add_c1 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("Alpha Closing Co".into()), PyRep::String("ALCL".into()), PyRep::Real(0.05)],
        &kw,
    )
    .await
    .expect("AddCorporation 1");
    let corp1 = add_c1.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp1");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp1).expect("corp1 rec");
        c.ensure_wallet_divisions();
        c.set_division_balance(1, IskCents::from_isk(100_000_000));
    });
    let rent1 = rpc(&ctx, &mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(JITA_STATION)], &kw)
        .await
        .expect("RentOffice 1");
    let office1 = rent1
        .as_i64()
        .or_else(|| rent1.dict_get("officeID").and_then(PyRep::as_i64))
        .unwrap_or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .values()
                    .find(|i| i.owner_id == corp1 && i.type_id == 27)
                    .map(|i| i.item_id)
                    .expect("office1 in db")
            })
        });

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let quit1 = rpc(&ctx, &mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(PILOT_1)], &kw)
        .await
        .expect("solo CEO KickOutMember");
    assert_eq!(quit1.dict_get("removed").and_then(PyRep::as_bool), Some(true));
    let n1 = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(
        count_item_notifs(&n1, office1, None, Some(6), None),
        1,
        "RPC corp closure must deliver office tombstone once to leaving pilot: {n1:?}"
    );

    // 2. Downtime path (no RPC): solo CEO corp rents office, pending_auto_kicks triggers execute_pending_auto_kicks_on_downtime.
    let add_c2 = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("Beta Closing Co".into()), PyRep::String("BTCL".into()), PyRep::Real(0.05)],
        &kw,
    )
    .await
    .expect("AddCorporation 2");
    let corp2 = add_c2.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp2");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp2).expect("corp2 rec");
        c.ensure_wallet_divisions();
        c.set_division_balance(1, IskCents::from_isk(100_000_000));
    });
    let rent2 = rpc(&ctx, &mut sess2, "officeManager", None, "RentOffice", &[PyRep::Integer(JITA_STATION)], &kw)
        .await
        .expect("RentOffice 2");
    let office2 = rent2
        .as_i64()
        .or_else(|| rent2.dict_get("officeID").and_then(PyRep::as_i64))
        .unwrap_or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .values()
                    .find(|i| i.owner_id == corp2 && i.type_id == 27)
                    .map(|i| i.item_id)
                    .expect("office2 in db")
            })
        });
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp2)
            .or_default()
            .pending_auto_kicks
            .insert(PILOT_2, 1);
    });
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    let dt_kicked = execute_pending_auto_kicks_on_downtime(&ctx);
    assert_eq!(dt_kicked, 1);
    let n2 = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(
        count_item_notifs(&n2, office2, None, Some(6), None),
        1,
        "Downtime corp closure (no RPC) must deliver office tombstone once: {n2:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_station_other_char_and_nonmember_corp_items_do_not_leak_to_acting_pilot() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let pilot_3 = 96_931_803_i64;
    let ship_3 = 1_931_803_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "pilot3".into(),
            eve_db::AccountRecord {
                user_id: 96_931_803,
                username: "pilot3".into(),
                password_token: WorldRepository::hash_password_token("pilot3", "demo-secret"),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters[&PILOT_1].clone();
        ch.character_id = pilot_3;
        ch.user_id = 96_931_803;
        ch.name = "Pilot Three".into();
        ch.corporation_id = 1_000_002;
        ch.station_id = Some(JITA_STATION);
        ch.structure_id = None;
        ch.solar_system_id = JITA_SYS;
        ch.active_ship_id = ship_3;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(pilot_3, ch);
        if let Some(p1_skills) = snap.skills.get(&PILOT_1).cloned() {
            snap.skills.insert(pilot_3, p1_skills);
        }
        snap.items.insert(
            ship_3,
            InventoryItemRecord {
                item_id: ship_3,
                type_id: 587,
                owner_id: pilot_3,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for cid in [PILOT_1, PILOT_2] {
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.station_id = Some(JITA_STATION);
                c.structure_id = None;
                c.solar_system_id = JITA_SYS;
                c.balance = IskCents::from_isk(500_000_000);
            }
        }
        snap.market_orders.clear();
    });

    let mut sess_a = login_select(&ctx, 73_501, "demo", "demo-secret", PILOT_1).await;
    let mut sess_b = login_select(&ctx, 73_502, "other", "other-secret", PILOT_2).await;
    let mut sess_c = login_select(&ctx, 73_503, "pilot3", "demo-secret", pilot_3).await;
    let kw = HashMap::new();

    // Pilot 3 founds Corp C (PILOT_1 is NOT a member of Corp C).
    let add_c = rpc(
        &ctx,
        &mut sess_c,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("External Trade Corp".into()), PyRep::String("EXTC".into()), PyRep::Real(0.05)],
        &kw,
    )
    .await
    .expect("AddCorporation C");
    let corp_c = add_c.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp_c");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_c).expect("corp_c");
        c.ensure_wallet_divisions();
        c.set_division_balance(1, IskCents::from_isk(200_000_000));
    });
    let r3 = rpc(&ctx, &mut sess_c, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1000)], &kw)
        .await
        .expect("SetAccountKey");
    assert!(matches!(r3, PyRep::None | PyRep::Integer(_) | PyRep::Bool(_)));

    let mut marketable = ctx
        .sde
        .types
        .values()
        .filter(|t| t.published && t.market_group_id.is_some() && t.category_id != 9)
        .map(|t| t.type_id)
        .collect::<Vec<_>>();
    marketable.sort_unstable();
    let tid_b = marketable.iter().copied().find(|&t| t == 34).unwrap_or(marketable[0]);
    let tid_c = marketable.iter().copied().find(|&t| t == 35).unwrap_or(marketable[1]);

    // Pre-existing station stacks for B and Corp C + resting buy orders placed by B and Corp C.
    let buy_b = rpc(
        &ctx,
        &mut sess_b,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(i64::from(tid_b)),
            PyRep::Real(50.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .expect("Pilot B PlaceBuyOrder");
    assert_eq!(buy_b.dict_get("filledImmediately").and_then(PyRep::as_bool), Some(false));

    let buy_c = rpc(
        &ctx,
        &mut sess_c,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(i64::from(tid_c)),
            PyRep::Real(50.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(true),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .expect("Corp C PlaceBuyOrder");
    assert_eq!(buy_c.dict_get("filledImmediately").and_then(PyRep::as_bool), Some(false));

    let a_sell_b = grant_next_item(&ctx, PILOT_1, JITA_STATION, tid_b, InventoryFlag::Hangar, 10);
    let a_sell_c = grant_next_item(&ctx, PILOT_1, JITA_STATION, tid_c, InventoryFlag::Hangar, 10);
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    let _ = ctx.drain_notifications_for_char(pilot_3);

    // Pilot A's RPCs sell into Pilot B's order and non-member Corp C's order, creating/updating
    // items owned by B and Corp C directly in JITA_STATION.
    let sell_entries = PyRep::List(vec![
        PyRep::keyval([
            ("itemID", PyRep::Integer(a_sell_b)),
            ("typeID", PyRep::Integer(i64::from(tid_b))),
            ("price", PyRep::Real(50.0)),
            ("quantity", PyRep::Integer(4)),
        ]),
        PyRep::keyval([
            ("itemID", PyRep::Integer(a_sell_c)),
            ("typeID", PyRep::Integer(i64::from(tid_c))),
            ("price", PyRep::Real(50.0)),
            ("quantity", PyRep::Integer(4)),
        ]),
    ]);
    let sell_rep = rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_entries, PyRep::Bool(false), PyRep::Integer(0)],
        &kw,
    )
    .await
    .expect("Pilot A PlaceMultiSellOrder");
    assert!(!format!("{sell_rep:?}").is_empty());

    let (b_item_ids, c_item_ids): (Vec<i64>, Vec<i64>) = ctx.db.with_snapshot(|snap| {
        let b_ids = snap
            .items
            .values()
            .filter(|i| i.owner_id == PILOT_2 && i.type_id == tid_b && i.location_id == JITA_STATION)
            .map(|i| i.item_id)
            .collect();
        let c_ids = snap
            .items
            .values()
            .filter(|i| i.owner_id == corp_c && i.type_id == tid_c && i.location_id == JITA_STATION)
            .map(|i| i.item_id)
            .collect();
        (b_ids, c_ids)
    });
    assert!(!b_item_ids.is_empty(), "Pilot B must hold purchased station stack");
    assert!(!c_item_ids.is_empty(), "Corp C must hold purchased station stack");

    let n_a = ctx.drain_notifications_for_char(PILOT_1);
    for notif in &n_a {
        if notif.event == "OnItemChange" {
            let iid = dbrow_col_i64(&notif.args[0], "itemID").unwrap_or(0);
            let oid = dbrow_col_i64(&notif.args[0], "ownerID").unwrap_or(0);
            assert!(
                !b_item_ids.contains(&iid) && oid != PILOT_2,
                "Pilot A must NEVER receive Pilot B's station item update: {notif:?}"
            );
            assert!(
                !c_item_ids.contains(&iid) && oid != corp_c,
                "Pilot A must NEVER receive non-member Corp C's station item update: {notif:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_outside_rpc_and_same_commit_ownerless_container_emit_zero_to_pilot() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let _sess = login_select(&ctx, 73_601, "demo", "demo-secret", PILOT_1).await;

    let loose_ownerless = 1_936_000_001_i64;
    insert_item_rec(&ctx, loose_ownerless, 34, 0, i64::from(JITA_SYS), InventoryFlag::Cargo, 50, false);
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // 1. Outside RPC: mutating existing ownerless item emits zero projected events.
    ctx.db
        .tx("it_outside_ownerless", |tx| -> Result<(), eve_db::TxError> {
            if let Some(it) = tx.snap.items.get_mut(&loose_ownerless) {
                it.quantity = 35;
                it.version += 1;
            }
            Ok(())
        })
        .expect("outside ownerless tx");
    assert!(
        ctx.drain_notifications_for_char(PILOT_1).is_empty(),
        "outside RPC ownerless change must produce 0 events"
    );

    // 2. Inside RPC: same commit creates ownerless space container and contents -> sends acting pilot nothing.
    let new_can = 1_936_000_010_i64;
    let new_loot = 1_936_000_011_i64;
    run_staged_dispatch_commit(&ctx, PILOT_1, || {
        ctx.db
            .tx("it_rpc_create_ownerless_can", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    new_can,
                    InventoryItemRecord {
                        item_id: new_can,
                        type_id: 23,
                        owner_id: 0,
                        location_id: i64::from(JITA_SYS),
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                tx.snap.items.insert(
                    new_loot,
                    InventoryItemRecord {
                        item_id: new_loot,
                        type_id: 34,
                        owner_id: 0,
                        location_id: new_can,
                        flag: InventoryFlag::Cargo,
                        quantity: 20,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("rpc ownerless creation");
    })
    .await;
    let pilot_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        pilot_notifs.is_empty(),
        "same-commit ownerless container + contents must send acting pilot nothing: {pilot_notifs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_own_item_moved_into_own_corp_hangar_emits_exactly_once_for_member() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let mut sess = login_select(&ctx, 73_701, "demo", "demo-secret", PILOT_1).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.balance = IskCents::from_isk(500_000_000);
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYS;
        }
    });
    sess.station_id = Some(JITA_STATION);

    let kw = HashMap::new();
    let add_c = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("Single Event Corp".into()), PyRep::String("SEVC".into()), PyRep::Real(0.05)],
        &kw,
    )
    .await
    .expect("AddCorporation");
    let corp_id = add_c.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp_id");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).expect("corp");
        c.ensure_wallet_divisions();
        c.set_division_balance(1, IskCents::from_isk(100_000_000));
    });
    let rent = rpc(&ctx, &mut sess, "officeManager", None, "RentOffice", &[PyRep::Integer(JITA_STATION)], &kw)
        .await
        .expect("RentOffice");
    let office_id = rent
        .as_i64()
        .or_else(|| rent.dict_get("officeID").and_then(PyRep::as_i64))
        .unwrap_or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .values()
                    .find(|i| i.owner_id == corp_id && i.type_id == 27)
                    .map(|i| i.item_id)
                    .expect("office in db")
            })
        });

    let bound_office = bind_inv(&ctx, &mut sess, JITA_STATION, 15, office_id).await;
    let own_trit = grant_next_item(&ctx, PILOT_1, JITA_STATION, 34, InventoryFlag::Hangar, 75);
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    let add_rep = rpc(
        &ctx,
        &mut sess,
        "invbroker",
        Some(bound_office),
        "Add",
        &[
            PyRep::Integer(own_trit),
            PyRep::Integer(office_id),
            PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
        ],
        &kw,
    )
    .await
    .expect("dispatcher Add own item to CorpSAG1");
    assert_eq!(add_rep.dict_get("itemID").and_then(PyRep::as_i64), Some(own_trit));

    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    let total_for_item = count_item_notifs(&notifs, own_trit, None, None, None);
    assert_eq!(
        total_for_item, 1,
        "member moving own item into own corp hangar must receive exactly ONE OnItemChange for it: {notifs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_quantum_core_install_suppresses_followup_hand_written_moved_event() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let perim_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == PERIMETER_SYS)
        .map(|s| s.station_id)
        .expect("Perimeter station");
    let mut sess = login_select(&ctx, 73_801, "demo", "demo-secret", PILOT_1).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = PERIMETER_SYS;
            ch.station_id = Some(perim_station);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(500_000_000);
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = perim_station;
            }
        }
    });
    sess.solar_system_id = Some(PERIMETER_SYS);
    sess.station_id = Some(perim_station);

    let kw = HashMap::new();
    let add_c = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("Core Ledger Corp".into()), PyRep::String("CRLD".into()), PyRep::Real(0.05)],
        &kw,
    )
    .await
    .expect("AddCorporation");
    let corp_id = add_c.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp_id");
    assert!(corp_id > 0);
    ctx.advance_virtual_clock(7 * 86_400 + 12);

    let undock = rpc(&ctx, &mut sess, "structureDocking", None, "Undock", &[], &kw)
        .await
        .expect("Undock");
    assert!(!format!("{undock:?}").is_empty());

    let st_pos = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == PERIMETER_SYS)
        .map(|s| s.position + DVec3::new(0.0, 5.0e9, 0.0))
        .expect("station pos");
    assert!(
        ctx.systems
            .get_or_spawn(PERIMETER_SYS)
            .set_ball_position(sess.ship_id.expect("ship"), st_pos)
            .await
    );

    let struct_type_id = 35_832_u32; // Astrahus
    grant_next_item(&ctx, PILOT_1, sess.ship_id.expect("ship"), struct_type_id, InventoryFlag::Cargo, 1);
    let anc = rpc(
        &ctx,
        &mut sess,
        "structureDeployment",
        None,
        "Anchor",
        &[PyRep::Integer(i64::from(struct_type_id)), PyRep::String("Core Proof Cit".into())],
        &kw,
    )
    .await
    .expect("Anchor");
    let structure_id = anc.dict_get("structureID").and_then(PyRep::as_i64).expect("structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    let guests_rep = rpc(&ctx, &mut sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(structure_id)], &kw)
        .await
        .expect("GetGuests");
    assert!(!format!("{guests_rep:?}").is_empty());

    let exact_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&structure_id].position));
    assert!(
        ctx.systems
            .get_or_spawn(PERIMETER_SYS)
            .set_ball_position(sess.ship_id.expect("ship"), exact_pos)
            .await
    );
    let dock_rep = rpc(&ctx, &mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(structure_id)], &kw)
        .await
        .expect("Dock structure");
    assert!(!format!("{dock_rep:?}").is_empty());

    let core_tid = ctx
        .sde
        .types
        .get(&struct_type_id)
        .and_then(|t| t.default_attributes.get(&life::ATTR_REQUIRES_DEED_TYPE))
        .copied()
        .expect("core tid") as u32;
    let core_item = grant_next_item(&ctx, PILOT_1, sess.ship_id.expect("ship"), core_tid, InventoryFlag::Cargo, 1);

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let add_core_rep = rpc(
        &ctx,
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(core_item), PyRep::Integer(structure_id), PyRep::Integer(172)],
        &kw,
    )
    .await
    .expect("dispatcher Add quantum core");
    assert_eq!(add_core_rep.dict_get("itemID").and_then(PyRep::as_i64), Some(core_item));

    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    let core_events: Vec<(Option<i64>, Option<i64>)> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange" && dbrow_col_i64(&n.args[0], "itemID") == Some(core_item))
        .map(|n| {
            (
                dbrow_col_i64(&n.args[0], "locationID"),
                dbrow_col_i64(&n.args[0], "flagID"),
            )
        })
        .collect();
    let consume_pos = core_events
        .iter()
        .position(|(loc, _)| *loc == Some(6))
        .unwrap_or_else(|| panic!("expected core consumption tombstone (loc 6): {core_events:?}"));
    assert_eq!(
        consume_pos + 1,
        core_events.len(),
        "no hand-written moved event may follow the quantum core consumption event: {core_events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3b1_salvage_rpc_and_tick_notify_foreign_and_unowned_wrecks() {
    let ctx = paused_ctx_with_sde(shared_sde());
    let mut salv_sess = login_select(&ctx, 73_801, "demo", "demo-secret", PILOT_1).await;
    let _owner_sess = login_select(&ctx, 73_802, "other", "other-secret", PILOT_2).await;

    let ship_id = salv_sess.ship_id.expect("salvager active ship");
    let salvager_id = 1_938_000_001_i64;
    let salvage_drone_id = 1_938_000_002_i64;
    let foreign_rpc_wreck = 1_938_000_010_i64;
    let foreign_rpc_loot = 1_938_000_011_i64;
    let unowned_tick_wreck = 1_938_000_020_i64;
    let unowned_tick_loot = 1_938_000_021_i64;
    let foreign_tick_wreck = 1_938_000_030_i64;
    let foreign_tick_loot = 1_938_000_031_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 626; // Vexor: HiSlot0..3 + DroneBay + DroneBandwidth
        }
        snap.items.retain(|_, item| {
            !(item.location_id == ship_id && matches!(item.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1))
        });
        let sk = snap.skills.entry(PILOT_1).or_default();
        for tid in [3436_u32, 3438_u32, 25_863_u32] {
            sk.insert(
                tid,
                eve_db::TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        eve_testkit::legal_fixture::ensure_legal_active_ship_skills_in_snap(
            snap,
            &ctx.sde,
            PILOT_1,
        );
        for (iid, tid, owner, loc, flag, qty, single) in [
            (salvager_id, 25_861_u32, PILOT_1, ship_id, InventoryFlag::HiSlot0, 1, true),
            (salvage_drone_id, 32_787_u32, PILOT_1, ship_id, InventoryFlag::DroneBay, 1, true),
            (foreign_rpc_wreck, 26_557_u32, PILOT_2, i64::from(JITA_SYS), InventoryFlag::None, 1, true),
            (foreign_rpc_loot, 34_u32, PILOT_2, foreign_rpc_wreck, InventoryFlag::Cargo, 45, false),
            (unowned_tick_wreck, 26_557_u32, 0, i64::from(JITA_SYS), InventoryFlag::None, 1, true),
            (unowned_tick_loot, 34_u32, 0, unowned_tick_wreck, InventoryFlag::Cargo, 60, false),
            (foreign_tick_wreck, 26_557_u32, PILOT_2, i64::from(JITA_SYS), InventoryFlag::None, 1, true),
            (foreign_tick_loot, 34_u32, PILOT_2, foreign_tick_wreck, InventoryFlag::Cargo, 75, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: single,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let kw = HashMap::new();
    let undock_rep = rpc(&ctx, &mut salv_sess, "structureDocking", None, "Undock", &[], &kw)
        .await
        .expect("Undock");
    assert!(!format!("{undock_rep:?}").is_empty());
    let sys_id = salv_sess.solar_system_id.expect("solar system");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_h
        .get_ball_snapshot(ship_id)
        .await
        .expect("ship ball query")
        .expect("undocked ship ball")
        .1
        .position;

    for (wid, pos_offset) in [
        (foreign_rpc_wreck, DVec3::new(900.0, 0.0, 0.0)),
        (unowned_tick_wreck, DVec3::new(1_000.0, 0.0, 0.0)),
        (foreign_tick_wreck, DVec3::new(1_100.0, 0.0, 0.0)),
    ] {
        let wpos = ship_pos + pos_offset;
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(wid, [wpos.x, wpos.y, wpos.z]);
        });
        sys_h
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: wid,
                    type_id: 26_557,
                    position: wpos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 25.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(26_557),
            )
            .await
            .expect("spawn wreck ball");
        sys_h
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: wid,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .expect("lock wreck");
    }

    // 1. Part (a): RPC salvage (`ship.LaunchDrones` + `entity.CmdSalvage`) on foreign player's wreck + loot.
    let launch_rep = rpc(
        &ctx,
        &mut salv_sess,
        "ship",
        None,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(salvage_drone_id)])],
        &kw,
    )
    .await
    .expect("LaunchDrones");
    assert!(!format!("{launch_rep:?}").is_empty());

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    ctx.mechanic_rng.set_seed(Some(3));
    let cmd_res = rpc(
        &ctx,
        &mut salv_sess,
        "entity",
        None,
        "CmdSalvage",
        &[PyRep::Integer(foreign_rpc_wreck)],
        &kw,
    )
    .await
    .expect("entity.CmdSalvage");
    assert_eq!(cmd_res.dict_get("salvaged").and_then(PyRep::as_bool), Some(true), "{cmd_res:?}");

    let rpc_can_id = ctx
        .db
        .with_snapshot(|s| s.items[&foreign_rpc_loot].location_id);
    assert_ne!(rpc_can_id, foreign_rpc_wreck);
    let salv_rpc_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(
        count_item_notifs(&salv_rpc_notifs, foreign_rpc_loot, Some(45), Some(rpc_can_id), None),
        1,
        "salvager must receive OnItemChange for moved foreign loot on RPC salvage: {salv_rpc_notifs:?}"
    );
    let owner_rpc_notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(
        count_item_notifs(&owner_rpc_notifs, foreign_rpc_wreck, None, Some(6), None),
        1,
        "foreign wreck owner must receive OnItemChange wreck tombstone on RPC salvage: {owner_rpc_notifs:?}"
    );
    assert_eq!(
        count_item_notifs(&owner_rpc_notifs, foreign_rpc_loot, Some(45), Some(rpc_can_id), None),
        1,
        "foreign wreck owner must receive OnItemChange for loot moved to jetcan: {owner_rpc_notifs:?}"
    );

    // 2. Part (b1): Tick-driven Salvager I (`dogmaIM.Activate` -> `step_manual` -> `handle_salvage_end_action`)
    //    on an unowned/NPC wreck (`owner_id = 0`) with loot.
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    ctx.mechanic_rng.set_seed(Some(3));
    rpc(
        &ctx,
        &mut salv_sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(salvager_id), PyRep::Integer(unowned_tick_wreck)],
        &kw,
    )
    .await
    .expect("Activate Salvager on unowned wreck");
    ctx.advance_secs(12);
    sys_h
        .step_manual(12.0)
        .await
        .expect("step unowned wreck salvage cycle");
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&unowned_tick_wreck)),
        "unowned tick wreck must be salvaged and removed"
    );
    let unowned_can_id = ctx
        .db
        .with_snapshot(|s| s.items[&unowned_tick_loot].location_id);
    assert_ne!(unowned_can_id, unowned_tick_wreck);
    let salv_unowned_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(
        count_item_notifs(&salv_unowned_notifs, unowned_tick_loot, Some(60), Some(unowned_can_id), None),
        1,
        "salvager must receive OnItemChange for moved unowned wreck loot on tick salvage: {salv_unowned_notifs:?}"
    );
    assert_eq!(
        count_item_notifs(&salv_unowned_notifs, unowned_tick_wreck, None, Some(6), None),
        1,
        "salvager must receive OnItemChange for deleted unowned wreck on tick salvage: {salv_unowned_notifs:?}"
    );

    // 3. Part (b2): Tick-driven Salvager I (`handle_salvage_end_action`) on a foreign-player wreck + loot.
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    ctx.mechanic_rng.set_seed(Some(3));
    rpc(
        &ctx,
        &mut salv_sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(salvager_id), PyRep::Integer(foreign_tick_wreck)],
        &kw,
    )
    .await
    .expect("Activate Salvager on foreign wreck");
    ctx.advance_secs(12);
    sys_h
        .step_manual(12.0)
        .await
        .expect("step foreign wreck salvage cycle");
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&foreign_tick_wreck)),
        "foreign tick wreck must be salvaged and removed"
    );
    let foreign_can_id = ctx
        .db
        .with_snapshot(|s| s.items[&foreign_tick_loot].location_id);
    assert_ne!(foreign_can_id, foreign_tick_wreck);
    let salv_foreign_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(
        count_item_notifs(&salv_foreign_notifs, foreign_tick_loot, Some(75), Some(foreign_can_id), None),
        1,
        "salvager must receive OnItemChange for moved foreign wreck loot on tick salvage: {salv_foreign_notifs:?}"
    );
    let owner_foreign_notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert_eq!(
        count_item_notifs(&owner_foreign_notifs, foreign_tick_wreck, None, Some(6), None),
        1,
        "foreign wreck owner must receive OnItemChange wreck tombstone on tick salvage: {owner_foreign_notifs:?}"
    );
    assert_eq!(
        count_item_notifs(&owner_foreign_notifs, foreign_tick_loot, Some(75), Some(foreign_can_id), None),
        1,
        "foreign wreck owner must receive OnItemChange for moved loot on tick salvage: {owner_foreign_notifs:?}"
    );
}
