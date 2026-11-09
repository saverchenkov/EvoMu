//! Live integration tests covering client-server notification & cache synchronization
//! parity across issues #738, #737, and #751.

use crate::common;
use eve_db::{corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR, ServerContext};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(77));
    ctx
}

async fn login_docked(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    station_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = SYS_JITA;
        }
    });
    s.station_id = Some(station_id);
    s.structure_id = None;
    s.solar_system_id = Some(SYS_JITA);
    s
}

fn assert_no_legacy_ix_quantity(notif: &eve_server::router::cache_notifications::PendingMachoNotification) {
    if notif.event != "OnItemChange" {
        return;
    }
    assert!(
        matches!(&notif.args[0], PyRep::DBRow { .. }),
        "OnItemChange args[0] must be a DBRow, got {:?}",
        notif.args[0]
    );
    if let Some(PyRep::Dict(entries)) = notif.args.get(1) {
        for (k, _) in entries {
            assert_ne!(
                k,
                &PyRep::Integer(5),
                "OnItemChange must never emit legacy ixQuantity (5): {:?}",
                entries
            );
        }
    }
}

fn dbrow_cell_i64(rep: &PyRep, col: &str) -> Option<i64> {
    rep.dbrow_get(col).and_then(|v| v.as_i64())
}

#[tokio::test]
async fn arch_738_seq_id_overflow_live_listener_cfg_sov_cache_and_corp_actions() {
    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 11_001, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_b = login_docked(&ctx, 11_002, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let kw = HashMap::new();

    // Setup shared player corp (A = CEO + Director, B = member).
    let corp_id = 98_700_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Arch Sync Corp".into();
        corp.ticker = "ASNC".into();
        corp.ceo_id = PILOT_A;
        corp.hq_station_id = Some(JITA_STATION);
        corp.members.insert(PILOT_A);
        corp.members.insert(PILOT_B);
        corp.set_division_balance(1, IskCents(100_000_000));
        corp.set_division_balance(2, IskCents(50_000_000));
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.corporation_id = corp_id;
        }
    });
    sess_a.corporation_id = Some(corp_id);
    sess_a.corp_role = corp_roles::ROLE_DIRECTOR;
    sess_b.corporation_id = Some(corp_id);
    sess_b.corp_role = 0;

    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 1a. Queue at cap (128): dispatch_rpc still captures notifications emitted during the call via seq_id.
    for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        ctx.notify_skill_trained(PILOT_A, 100 + i as i64, 3300, 1);
    }
    assert_eq!(ctx.drain_notifications_for_char(PILOT_A).len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        ctx.notify_skill_trained(PILOT_A, 200 + i as i64, 3300, 1);
    }
    sess_a.notifications.clear();
    let skill_item_id = 980_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            skill_item_id,
            InventoryItemRecord {
                item_id: skill_item_id,
                type_id: 3400, // Spaceship Command
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .dispatch_rpc(
            &mut sess_a,
            "dogmaIM",
            None,
            "InjectSkillIntoBrain",
            &[PyRep::Integer(skill_item_id)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillIntoBrain");
    assert!(
        sess_a.notifications.iter().any(|(ev, _)| ev == "OnItemChange"),
        "dispatch_rpc must deliver new notifications even when pending queue was already at MAX_PENDING_NOTIFICATIONS_PER_CHAR"
    );
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // 1b. live_notify_listener = true suppresses duplicate session.notifications collection during dispatch_rpc.
    sess_a.live_notify_listener = true;
    sess_a.notifications.clear();
    let skill_item_id2 = 980_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            skill_item_id2,
            InventoryItemRecord {
                item_id: skill_item_id2,
                type_id: 3402, // Science
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .dispatch_rpc(
            &mut sess_a,
            "dogmaIM",
            None,
            "InjectSkillIntoBrain",
            &[PyRep::Integer(skill_item_id2)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillIntoBrain 2");
    assert!(
        sess_a.notifications.is_empty(),
        "live_notify_listener=true must not push duplicate notifications into session.notifications"
    );
    let pending_a = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        pending_a.iter().any(|n| n.event == "OnItemChange"),
        "pending queue still holds notifications for the live listener"
    );
    sess_a.live_notify_listener = false;

    // 2. OnCfgDataChanged("evelocations", ...) 7-element format on structure anchor/online notification emitter
    eve_server::services::notify_emitters_space_dungeons::emit_structure_cfg_and_jammer_notifications(
        &ctx,
        SYS_JITA,
        88_000_001,
        35_832,
        "Test Hub",
        [100.0, 200.0, 300.0],
        false,
    );
    let cfg_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let cfg = cfg_notifs
        .iter()
        .find(|n| n.event == "OnCfgDataChanged")
        .expect("OnCfgDataChanged emitted");
    assert_eq!(cfg.args[0], PyRep::String("evelocations".into()));
    match &cfg.args[1] {
        PyRep::List(elems) => {
            assert_eq!(elems.len(), 7, "OnCfgDataChanged evelocations row must have 7 columns");
            assert_eq!(elems[0], PyRep::Integer(88_000_001));
            assert_eq!(elems[1], PyRep::String("Test Hub".into()));
            assert_eq!(elems[2], PyRep::Integer(i64::from(SYS_JITA)));
        }
        other => panic!("expected 7-element List, got {other:?}"),
    }

    // 3. stationSvc::GetAllianceSystems cache invalidation on sov change & sov hub destruction.
    let oid = ServerContext::method_call_object_id("stationSvc", "GetAllianceSystems");
    let _ = ctx.get_or_populate_cached_object(&oid, true);
    eve_server::services::sov_services::emit_sovereignty_changed(&ctx, SYS_JITA, Some(99_000_001), Some(corp_id));
    assert!(
        !ctx.invalidate_cached_method("stationSvc", "GetAllianceSystems"),
        "emit_sovereignty_changed must invalidate stationSvc::GetAllianceSystems cache"
    );
    let _ = ctx.get_or_populate_cached_object(&oid, true);
    let hub_id = 88_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32_458, // Sovereignty Hub / Infrastructure Hub
                owner_corp_id: corp_id,
                solar_system_id: SYS_JITA,
                name: "Jita Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        eve_server::services::station_structure::structure_lifecycle::destroy_structure(
            &ctx,
            snap,
            &mut ext,
            hub_id,
            PILOT_B,
            ctx.now_unix(),
        );
    });
    assert!(
        !ctx.invalidate_cached_method("stationSvc", "GetAllianceSystems"),
        "Destroying a Sovereignty Hub must invalidate stationSvc::GetAllianceSystems cache"
    );
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 4. corpRegistry.ExecuteActions cross-pilot session sync & OnCorporationMemberChanged
    let action_res = ctx
        .dispatch_rpc(
            &mut sess_a,
            "corpRegistry",
            None,
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(PILOT_B)]),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("add".into()),
                    PyRep::String("roles".into()),
                    PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert!(action_res.is_ok(), "ExecuteActions add role failed: {action_res:?}");
    let b_notifs = ctx.drain_notifications_for_char(PILOT_B);
    assert!(
        b_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "ExecuteActions must emit OnSessionChanged when target member roles change"
    );
    assert!(
        b_notifs.iter().any(|n| n.event == "OnCorporationMemberChanged"),
        "ExecuteActions must emit OnCorporationMemberChanged to corporation members"
    );
    // Verify dispatch_rpc syncs PILOT_B's live session corp_role automatically via forced_session_corp_sync.
    let _ = ctx
        .dispatch_rpc(&mut sess_b, "corpRegistry", None, "GetCorporation", &[], &kw, None)
        .await
        .expect("GetCorporation sync");
    assert_ne!(
        sess_b.corp_role & corp_roles::ROLE_ACCOUNTANT,
        0,
        "forced_session_corp_sync must update PILOT_B live session corp_role on next RPC"
    );
}

#[tokio::test]
async fn arch_737_751_item_wallet_and_market_notification_parity() {
    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 12_001, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_b = login_docked(&ctx, 12_002, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let kw = HashMap::new();

    let corp_id = 98_700_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Commerce Parity Corp".into();
        corp.ticker = "CPC".into();
        corp.ceo_id = PILOT_A;
        corp.hq_station_id = Some(JITA_STATION);
        corp.members.insert(PILOT_A);
        corp.members.insert(PILOT_B);
        corp.set_division_balance(1, IskCents(500_000_000));
        corp.set_division_balance(2, IskCents(100_000_000));
        snap.corporations.insert(corp_id, corp);
        for pid in [PILOT_A, PILOT_B] {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.corporation_id = corp_id;
                ch.balance = IskCents(500_000_000);
            }
        }
    });
    sess_a.corporation_id = Some(corp_id);
    sess_a.corp_role = corp_roles::ROLE_DIRECTOR;
    sess_b.corporation_id = Some(corp_id);
    sess_b.corp_role = 0;

    // 1. invbroker corp hangar routing & zero legacy ixQuantity (5)
    let corp_src_item = 990_001_i64;
    let personal_item = 990_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            corp_src_item,
            InventoryItemRecord {
                item_id: corp_src_item,
                type_id: 34, // Tritanium
                owner_id: corp_id,
                location_id: JITA_STATION,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            personal_item,
            InventoryItemRecord {
                item_id: personal_item,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // Move personal hangar item into CorpSAG1 -> notifies corporation scope (PILOT_B receives corpid OnItemChange) with no ixQuantity (5).
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(personal_item),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Add personal -> CorpSAG1");
    let bn_corp = ctx.drain_notifications_for_char(PILOT_B);
    for n in &bn_corp {
        assert_no_legacy_ix_quantity(n);
    }
    assert!(
        bn_corp.iter().any(|n| n.event == "OnItemChange" && n.id_type == "corpid"),
        "Moving item into CorpSAG1 must emit OnItemChange to corpid scope"
    );

    // Move CorpSAG1 item out to PILOT_A personal Hangar -> notifies BOTH corpid (removal from corp hangar) and charid!
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(corp_src_item),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Add CorpSAG1 -> personal Hangar");
    let bn_take = ctx.drain_notifications_for_char(PILOT_B);
    let an_take = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        bn_take.iter().any(|n| n.event == "OnItemChange" && n.id_type == "corpid"),
        "Taking item out of CorpSAG1 must notify corporation members"
    );
    assert!(
        an_take.iter().any(|n| n.event == "OnItemChange"),
        "Taking item into personal hangar must notify PILOT_A"
    );

    // 2. Direct station trade MakeOffer -> OnAccountChange for both parties + DBRow OnItemChange (including split stacks)
    let trade_item = 990_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            trade_item,
            InventoryItemRecord {
                item_id: trade_item,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let init_tr = ctx
        .dispatch_rpc(&mut sess_a, "trademgr", None, "InitiateTrade", &[PyRep::Integer(PILOT_B)], &kw, None)
        .await
        .expect("InitiateTrade");
    let trade_id = init_tr.dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut sess_a,
        "trademgr",
        None,
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(trade_item), PyRep::Integer(20)],
        &kw,
        None,
    )
    .await
    .expect("AddItem partial split");
    ctx.dispatch_rpc(
        &mut sess_b,
        "trademgr",
        None,
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(25_000.0)],
        &kw,
        None,
    )
    .await
    .expect("OfferMoney");
    ctx.dispatch_rpc(&mut sess_a, "trademgr", None, "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)], &kw, None)
        .await
        .expect("MakeOffer A");
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_time_seconds(301);
    ctx.dispatch_rpc(&mut sess_b, "trademgr", None, "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)], &kw, None)
        .await
        .expect("MakeOffer B");
    let tr_a = ctx.drain_notifications_for_char(PILOT_A);
    let tr_b = ctx.drain_notifications_for_char(PILOT_B);
    assert!(tr_a.iter().any(|n| n.event == "OnAccountChange"), "Trade completion must notify A wallet");
    assert!(tr_b.iter().any(|n| n.event == "OnAccountChange"), "Trade completion must notify B wallet");
    assert!(
        tr_b.iter().any(|n| n.event == "OnItemChange" && dbrow_cell_i64(&n.args[0], "quantity") == Some(20)),
        "Split trade item stack must emit DBRow OnItemChange to recipient"
    );
    for n in tr_a.iter().chain(tr_b.iter()) {
        assert_no_legacy_ix_quantity(n);
    }

    // 3. Contracts CreateContract + AcceptContract -> OnItemChange & OnAccountChange
    let contract_item = 990_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            contract_item,
            InventoryItemRecord {
                item_id: contract_item,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 75,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    let c_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(1), // Personal
                PyRep::Integer(PILOT_B),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(JITA_STATION),
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("Swap".into()),
                PyRep::String(String::new()),
                PyRep::List(vec![PyRep::Integer(contract_item)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateContract");
    let cid = c_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();
    let cr_a = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        cr_a.iter().any(|n| n.event == "OnItemChange"),
        "CreateContract escrowing items must emit OnItemChange"
    );
    assert!(
        cr_a.iter().any(|n| n.event == "OnAccountChange"),
        "CreateContract charging broker/deposit must emit OnAccountChange"
    );
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.dispatch_rpc(&mut sess_b, "contractProxy", None, "AcceptContract", &[PyRep::Integer(cid)], &kw, None)
        .await
        .expect("AcceptContract");
    let ac_a = ctx.drain_notifications_for_char(PILOT_A);
    let ac_b = ctx.drain_notifications_for_char(PILOT_B);
    assert!(ac_a.iter().any(|n| n.event == "OnAccountChange"), "AcceptContract must emit OnAccountChange to issuer");
    assert!(ac_b.iter().any(|n| n.event == "OnAccountChange"), "AcceptContract must emit OnAccountChange to acceptor");
    assert!(ac_b.iter().any(|n| n.event == "OnItemChange"), "AcceptContract must emit OnItemChange to acceptor");

    // 4. Account GiveCashFromCorpAccount (internal corp div + external char)
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_time_seconds(301);
    ctx.dispatch_rpc(
        &mut sess_a,
        "account",
        None,
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(corp_id),
            PyRep::Real(1_000_000.0),
            PyRep::Integer(1000),
            PyRep::Integer(1001),
            PyRep::String("Internal div transfer".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("GiveCashFromCorpAccount internal");
    let wn = ctx.drain_notifications_for_char(PILOT_B);
    assert!(
        wn.iter().any(|n| n.event == "OnAccountChange" && n.args[0] == PyRep::String("cash".into())),
        "Internal corp transfer must emit OnAccountChange(cash) for source div1"
    );
    assert!(
        wn.iter().any(|n| n.event == "OnAccountChange" && n.args[0] == PyRep::String("cash2".into())),
        "Internal corp transfer must emit OnAccountChange(cash2) for dest div2"
    );

    // 5. Market: Multi-sell, BuyMultipleItems counterparty OnOwnOrdersChanged, ModifyCharOrder cross, Corp order routing, Expiry
    let sell_a1 = 990_030_i64;
    let sell_a2 = 990_031_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 35);
        snap.items.insert(
            sell_a1,
            InventoryItemRecord {
                item_id: sell_a1,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            sell_a2,
            InventoryItemRecord {
                item_id: sell_a2,
                type_id: 35, // Pyerite
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let ms_res = ctx
        .dispatch_rpc(
            &mut sess_a,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION)),
                        ("typeID", PyRep::Integer(34)),
                        ("price", PyRep::Real(10.0)),
                        ("quantity", PyRep::Integer(500)),
                    ]),
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION)),
                        ("typeID", PyRep::Integer(35)),
                        ("price", PyRep::Real(20.0)),
                        ("quantity", PyRep::Integer(500)),
                    ]),
                ]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder 2 items");
    let placed_oids = ms_res.dict_get("orderIDs").expect("orderIDs");
    let PyRep::List(oid_list) = placed_oids else { panic!("expected List") };
    assert_eq!(oid_list.len(), 2);
    let ms_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(
        ms_notifs.iter().filter(|n| n.event == "OnOwnOrdersChanged").count(),
        2,
        "PlaceMultiSellOrder with 2 items must emit OnOwnOrdersChanged for each order"
    );

    // BuyMultipleItems filling PILOT_A's Pyerite sell order notifies PILOT_A with OnOwnOrdersChanged("Change")
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_time_seconds(301);
    ctx.dispatch_rpc(
        &mut sess_b,
        "marketProxy",
        None,
        "BuyMultipleItems",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(35)),
                ("quantity", PyRep::Integer(100)),
                ("maxPrice", PyRep::Real(25.0)),
                ("stationID", PyRep::Integer(JITA_STATION)),
            ])]),
            PyRep::Bool(false),
        ],
        &kw,
        None,
    )
    .await
    .expect("BuyMultipleItems");
    let sell_fill_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        sell_fill_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"),
        "BuyMultipleItems filling non-NPC order must emit OnOwnOrdersChanged to resting seller"
    );

    // ModifyCharOrder crossing a resting order notifies crossed counterparty with OnOwnOrdersChanged
    let mut buy_kw = HashMap::new();
    buy_kw.insert("useCorp".to_string(), PyRep::Bool(false));
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let buy_res = ctx
        .dispatch_rpc(
            &mut sess_b,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(35),
                PyRep::Real(5.0),
                PyRep::Integer(50),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
            ],
            &buy_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder below ask");
    let buy_oid = buy_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_time_seconds(301);
    ctx.dispatch_rpc(
        &mut sess_b,
        "marketProxy",
        None,
        "ModifyCharOrder",
        &[PyRep::Integer(buy_oid), PyRep::Real(20.0)],
        &kw,
        None,
    )
    .await
    .expect("ModifyCharOrder cross ask");
    let crossed_a = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        crossed_a.iter().any(|n| n.event == "OnOwnOrdersChanged"),
        "ModifyCharOrder crossing resting sell order must emit OnOwnOrdersChanged to seller"
    );

    // Expired order sweep emits OnOwnOrdersChanged("Expiry") + OnAccountChange + OnItemChange
    let exp_buy_res = ctx
        .dispatch_rpc(
            &mut sess_b,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(34),
                PyRep::Real(2.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(1), // 1 day duration
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder for expiry test");
    let _exp_oid = exp_buy_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.advance_time_seconds(90_000);
    ctx.dispatch_rpc(&mut sess_b, "marketProxy", None, "GetCharOrders", &[], &kw, None)
        .await
        .expect("GetCharOrders triggers expiry sweep");
    let exp_notifs = ctx.drain_notifications_for_char(PILOT_B);
    assert!(
        exp_notifs
            .iter()
            .any(|n| n.event == "OnOwnOrdersChanged" && n.args.get(1) == Some(&PyRep::String("Expiry".into()))),
        "Expired buy order sweep must emit OnOwnOrdersChanged(Expiry)"
    );
    assert!(
        exp_notifs.iter().any(|n| n.event == "OnAccountChange"),
        "Expired buy order escrow refund must emit OnAccountChange"
    );
}
