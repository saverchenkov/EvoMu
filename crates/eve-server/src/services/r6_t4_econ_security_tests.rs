//! Security regression and exploit tests for Round-6 Track 4 (`#402`, `#405`, `#406`, `#407`, `#408`, `#409`).

#![forbid(unsafe_code)]

use crate::router::client_compat::{normalize_macho_notification, normalize_macho_rpc_reply};
use crate::router::ServerContext;
use eve_db::{
    corp_roles, service_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderRecord, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_STATION: i64 = 60_003_760;
const JITA_SYSTEM: u32 = 30_000_142;
const THE_FORGE: u32 = 10_000_002;

fn test_ctx() -> ServerContext {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_repository(db)
}

fn ensure_test_char(
    s: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    name: &str,
    corp_id: i64,
    station_id: i64,
    balance_isk: i64,
) {
    let min_existing = s.characters.keys().min().copied().expect("demo seed character");
    let mut ch = s.characters[&min_existing].clone();
    ch.character_id = char_id;
    ch.user_id = char_id as u64;
    ch.name = name.into();
    ch.corporation_id = corp_id;
    ch.station_id = Some(station_id);
    ch.balance = IskCents::from_isk(balance_isk);
    ch.standings.clear();
    s.skills.remove(&char_id);
    s.characters.insert(char_id, ch);
}

fn make_session(user_id: u64, char_id: i64, corp_id: i64, station_id: i64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(88_000 + user_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.corp_role = corp_role;
    s.station_id = Some(station_id);
    s.solar_system_id = Some(JITA_SYSTEM);
    s.region_id = Some(THE_FORGE);
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

#[test]
fn test_402_quasar_normalization_preserves_authoritative_backend_replies_and_notices() {
    let real_planets = PyRep::rowset(
        vec!["planetID", "solarSystemID", "commandCenterLevel", "pinsCount"],
        vec![vec![
            PyRep::Integer(40_009_999),
            PyRep::Integer(30_002_187),
            PyRep::Integer(4),
            PyRep::Integer(7),
        ]],
    );
    let norm_planets =
        normalize_macho_rpc_reply("planetMgr", "GetPlanetsForChar", Ok(real_planets)).unwrap();
    let PyRep::List(planet_items) = norm_planets else { panic!("expected planet list"); };
    assert_eq!(planet_items.len(), 1);
    assert_eq!(planet_items[0].dict_get("planetID"), Some(&PyRep::Integer(40_009_999)));
    assert_eq!(planet_items[0].dict_get("numberOfPins"), Some(&PyRep::Integer(7)));
    assert!(planet_items[0].dict_get("colonyData").is_some());

    let quotes_reply = PyRep::Tuple(vec![
        PyRep::Real(0.02),
        PyRep::Real(0.55),
        PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(777_001)),
            ("outputs", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(250))])])),
        ])]),
    ]);
    let norm_quotes = normalize_macho_rpc_reply("reprocessingSvc", "GetQuotes", Ok(quotes_reply)).unwrap();
    let PyRep::Tuple(q_parts) = norm_quotes else { panic!("expected tuple"); };
    let PyRep::Dict(q_entries) = &q_parts[2] else { panic!("expected quotes dict"); };
    assert_eq!(q_entries.len(), 1);
    assert_eq!(q_entries[0].0, PyRep::Integer(777_001));

    let clone_state = PyRep::keyval([
        (
            "clones",
            PyRep::rowset(
                vec!["jumpCloneID", "locationID", "cloneName"],
                vec![vec![
                    PyRep::Integer(444_001),
                    PyRep::Integer(60_005_123),
                    PyRep::String("CustomClone".into()),
                ]],
            ),
        ),
        ("implants", PyRep::rowset(vec!["jumpCloneID", "implantID", "typeID"], vec![])),
        ("timeLastJump", PyRep::None),
    ]);
    assert_eq!(
        normalize_macho_rpc_reply("jumpCloneSvc", "GetCloneState", Ok(clone_state.clone())).unwrap(),
        clone_state
    );

    let alsc = normalize_macho_rpc_reply(
        "invbroker",
        "ALSCLogGet",
        Ok(PyRep::keyval([("entries", PyRep::List(vec![PyRep::keyval([("actorID", PyRep::Integer(90_000_555))])]))])),
    )
    .unwrap();
    let PyRep::List(alsc_rows) = alsc else { panic!("expected list"); };
    assert_eq!(alsc_rows[0].dict_get("actorID"), Some(&PyRep::Integer(90_000_555)));

    let fit = normalize_macho_rpc_reply(
        "ship",
        "GetShipFittingInfo",
        Ok(PyRep::keyval([
            ("cpuLoad", PyRep::Real(44.5)),
            ("cpuOutput", PyRep::Real(210.0)),
            ("powerLoad", PyRep::Real(18.0)),
            ("powerOutput", PyRep::Real(65.0)),
        ])),
    )
    .unwrap();
    assert_eq!(fit, PyRep::Tuple(vec![PyRep::Real(44.5), PyRep::Real(210.0), PyRep::Real(18.0), PyRep::Real(65.0)]));

    let war_info = normalize_macho_rpc_reply(
        "warStatisticMgr",
        "GetBaseInfo",
        Ok(PyRep::keyval([
            ("warID", PyRep::Integer(9123)),
            ("declaredByID", PyRep::Integer(98_000_111)),
            ("againstID", PyRep::Integer(98_000_222)),
            ("killsForAggressor", PyRep::Integer(14)),
            ("killsForDefender", PyRep::Integer(3)),
            ("iskDestroyedByAggressor", PyRep::Real(1_250_000.0)),
            ("iskDestroyedByDefender", PyRep::Real(400_000.0)),
        ])),
    )
    .unwrap();
    let PyRep::Tuple(war_parts) = war_info else { panic!("expected war tuple"); };
    assert_eq!(war_parts[0].dict_get("warID"), Some(&PyRep::Integer(9123)));

    let bm_notif = vec![PyRep::List(vec![PyRep::keyval([("folderID", PyRep::Integer(8801))])])];
    assert_eq!(normalize_macho_notification("OnSharedBookmarksFolderUpdated", bm_notif.clone()), bm_notif);
    let mod_notif = normalize_macho_notification("OnModuleAttributeChanges", vec![PyRep::List(vec![PyRep::Integer(700_009)])]);
    assert!(matches!(
        mod_notif.as_slice(),
        [PyRep::List(items)] if matches!(items.as_slice(), [PyRep::Tuple(t)] if t.len() == 8 && t[2] == PyRep::Integer(700_009))
    ));
    let inv_notif = normalize_macho_notification(
        "ProcessInventoryChange",
        vec![PyRep::Integer(900_001), PyRep::Integer(JITA_STATION), PyRep::Integer(5)],
    );
    assert!(matches!(
        inv_notif.as_slice(),
        [PyRep::List(items), PyRep::Dict(_), PyRep::Bool(true), PyRep::None]
            if items.first().and_then(|i| i.dict_get("itemID")) == Some(&PyRep::Integer(900_001))
    ));
    let inv_notif_4 = vec![PyRep::List(Vec::new()), PyRep::Dict(Vec::new()), PyRep::Bool(true), PyRep::None];
    assert_eq!(normalize_macho_notification("ProcessInventoryChange", inv_notif_4.clone()), inv_notif_4);
}

#[test]
fn test_405_and_406_contracts_corp_courier_and_public_deposit_refund() {
    let ctx = test_ctx();
    let issuer_id = 90_000_101_i64;
    let hauler_char = 90_000_102_i64;
    let hauler_corp = 98_000_102_i64;
    let ship_id = 700_101_i64;
    let courier_item_1 = 700_102_i64;
    let courier_item_2 = 700_103_i64;
    let ie_item = 700_104_i64;

    ctx.db.with_mut_snapshot(|s| {
        ensure_test_char(s, issuer_id, "IssuerPilot", 1_000_035, JITA_STATION, 5_000_000);
        ensure_test_char(s, hauler_char, "HaulerExec", hauler_corp, JITA_STATION, 1_000_000);
        if let Some(ch) = s.characters.get_mut(&hauler_char) {
            ch.active_ship_id = ship_id;
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = hauler_corp;
            __corp.name = "HaulCorp".into();
            __corp.ticker = "HAUL".into();
            __corp.ceo_id = hauler_char;
            __corp.set_division_balance(1, IskCents::from_isk(2_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        s.corporations.insert(hauler_corp, corp);
        s.items.insert(
            ship_id,
            InventoryItemRecord { item_id: ship_id, type_id: 650, owner_id: hauler_char, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() },
        );
        for (iid, qty) in [(courier_item_1, 10), (courier_item_2, 5), (ie_item, 1)] {
            s.items.insert(
                iid,
                InventoryItemRecord { item_id: iid, type_id: 34, owner_id: issuer_id, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: qty, singleton: false, version: 1, ..Default::default() },
            );
        }
    });

    let mut issuer_sess = make_session(101, issuer_id, 1_000_035, JITA_STATION, 0);
    let mut hauler_sess = make_session(102, hauler_char, hauler_corp, JITA_STATION, corp_roles::ROLE_DIRECTOR);
    hauler_sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // #406: Public ItemExchange refunds 1% creation deposit on AcceptContract
    let issuer_bal_start = ctx.db.with_snapshot(|s| s.characters[&issuer_id].balance);
    let ie_create = crate::services::contracts_service::handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION)),
            ("endStationID", PyRep::Integer(JITA_STATION)),
            ("price", PyRep::Real(1_000_000.0)),
            ("reward", PyRep::Real(0.0)),
            ("collateral", PyRep::Real(0.0)),
            ("numDays", PyRep::Integer(14)),
            ("itemList", PyRep::List(vec![PyRep::Integer(ie_item)])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let ie_cid = ie_create.dict_get("contractID").and_then(PyRep::as_i64).unwrap();
    let expected_deposit_isk = ie_create.dict_get("depositISK").and_then(PyRep::as_f64).unwrap_or(10_000.0);
    let expected_fee_isk = ie_create.dict_get("brokerFeeISK").and_then(PyRep::as_f64).unwrap_or(10_000.0);
    crate::services::contracts_service::handle(
        &ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(ie_cid)], &kw,
    ).unwrap().unwrap();
    let (issuer_bal_after_ie, deposit_remaining) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&issuer_id].balance,
            s.service_state_blobs.contains_key(&format!("contract_deposit:{ie_cid}")),
        )
    });
    let expected_after_ie = IskCents(
        issuer_bal_start.0 + IskCents::from_isk(1_000_000).0 - IskCents::from_f64_isk(expected_fee_isk).0,
    );
    assert!(expected_deposit_isk > 0.0);
    assert!(!deposit_remaining);
    assert_eq!(issuer_bal_after_ie, expected_after_ie);

    // #405: Corporate courier AcceptContract -> CompleteContract credits corp division 1 + syncs master_wallet
    let c1_create = crate::services::contracts_service::handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION)),
            ("endStationID", PyRep::Integer(JITA_STATION)),
            ("price", PyRep::Real(0.0)),
            ("reward", PyRep::Real(250_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("numDays", PyRep::Integer(14)),
            ("itemList", PyRep::List(vec![PyRep::Integer(courier_item_1)])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let cid_1 = c1_create.dict_get("contractID").and_then(PyRep::as_i64).unwrap();
    crate::services::contracts_service::handle(
        &ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_1), PyRep::Bool(true)], &kw,
    ).unwrap().unwrap();
    let (corp_after_accept, wrap_owner_1) = ctx.db.with_snapshot(|s| {
        let wrap_id = s.items[&courier_item_1].location_id;
        (s.corporations[&hauler_corp].division_balance(1), s.items[&wrap_id].owner_id)
    });
    assert_eq!(corp_after_accept, IskCents::from_isk(1_500_000));
    assert_eq!(wrap_owner_1, hauler_corp);

    crate::services::contracts_service::handle(
        &ctx, &mut hauler_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid_1)], &kw,
    ).unwrap().unwrap();
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.contracts[&cid_1].status, "Completed");
        let corp = &s.corporations[&hauler_corp];
        assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(2_250_000));
        assert_eq!(corp.division_balance(1), IskCents::from_isk(2_250_000));
    });

    // #405: BreakPlasticWrap & FailContract support corporate acceptor
    let c2_create = crate::services::contracts_service::handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION)),
            ("endStationID", PyRep::Integer(JITA_STATION)),
            ("price", PyRep::Real(0.0)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(300_000.0)),
            ("numDays", PyRep::Integer(14)),
            ("itemList", PyRep::List(vec![PyRep::Integer(courier_item_2)])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let cid_2 = c2_create.dict_get("contractID").and_then(PyRep::as_i64).unwrap();
    crate::services::contracts_service::handle(
        &ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_2), PyRep::Bool(true)], &kw,
    ).unwrap().unwrap();
    let wrap_2 = ctx.db.with_snapshot(|s| s.items[&courier_item_2].location_id);
    let break_res = crate::services::contracts_service::handle(
        &ctx, &mut hauler_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_2)], &kw,
    ).unwrap().unwrap();
    assert_eq!(break_res.dict_get("status"), Some(&PyRep::String("Failed".into())));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&courier_item_2].owner_id, hauler_corp);
        assert_eq!(s.items[&courier_item_2].location_id, JITA_STATION);
        assert!(!s.items.contains_key(&wrap_2));
    });
}

#[test]
fn test_407_wallet_give_cash_and_bill_payments_sync_corp_div1_and_enforce_debtor() {
    let ctx = test_ctx();
    let char_a = 90_000_201_i64;
    let char_b = 90_000_202_i64;
    let corp_debtor = 98_000_201_i64;
    let corp_creditor = 98_000_202_i64;

    ctx.db.with_mut_snapshot(|s| {
        ensure_test_char(s, char_a, "PilotA", corp_debtor, JITA_STATION, 5_000_000);
        ensure_test_char(s, char_b, "PilotB", corp_creditor, JITA_STATION, 1_000_000);
        for (cid, name, bal) in [(corp_debtor, "DebtCorp", 3_000_000_i64), (corp_creditor, "CredCorp", 500_000_i64)] {
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.name = name.into();
                __corp.ticker = "CORP".into();
                __corp.ceo_id = char_a;
                __corp.set_division_balance(1, IskCents::from_isk(bal));
                __corp };
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_isk(bal);
            s.corporations.insert(cid, corp);
        }
    });
    let bill_hijack = crate::services::wallet_serverops::create_bill(&ctx, 1, char_b, corp_creditor, 200_000.0, 2_000_000_000, 0.0, 0, 0);
    let bill_own = crate::services::wallet_serverops::create_bill(&ctx, 1, char_a, corp_creditor, 250_000.0, 2_000_000_000, 0.0, 0, 0);
    let bill_corp = crate::services::wallet_serverops::create_bill(&ctx, 2, corp_debtor, corp_creditor, 300_000.0, 2_000_000_000, 0.0, 0, 0);

    let mut sess_a = make_session(201, char_a, corp_debtor, JITA_STATION, corp_roles::ROLE_ACCOUNTANT);
    let kw = HashMap::new();

    // GiveCash credits corporate recipient's Division 1 + master_wallet
    crate::services::wallet_serverops::handle(
        &ctx, &mut sess_a, "account", "GiveCash",
        &[PyRep::Integer(corp_creditor), PyRep::Real(100_000.0), PyRep::String("grant".into())], &kw,
    ).unwrap().unwrap();
    ctx.db.with_snapshot(|s| {
        let cred = &s.corporations[&corp_creditor];
        assert_eq!(cred.master_wallet_balance(), IskCents::from_isk(600_000));
        assert_eq!(cred.division_balance(1), IskCents::from_isk(600_000));
    });

    // CharPayBill rejects paying another pilot's bill
    let hijack = crate::services::wallet_serverops::handle(
        &ctx, &mut sess_a, "billMgr", "CharPayBill", &[PyRep::Integer(bill_hijack)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(hijack.code, "AccessDenied");

    // CharPayBill succeeds for own bill and updates corporate creditor Division 1
    crate::services::wallet_serverops::handle(
        &ctx, &mut sess_a, "billMgr", "CharPayBill", &[PyRep::Integer(bill_own)], &kw,
    ).unwrap().unwrap();
    // PayCorporationBill updates corporate debtor & creditor Division 1 + master_wallet
    crate::services::wallet_serverops::handle(
        &ctx, &mut sess_a, "billMgr", "PayCorporationBill", &[PyRep::Integer(bill_corp)], &kw,
    ).unwrap().unwrap();
    ctx.db.with_snapshot(|s| {
        let debt = &s.corporations[&corp_debtor];
        let cred = &s.corporations[&corp_creditor];
        assert_eq!(debt.master_wallet_balance(), IskCents::from_isk(2_700_000));
        assert_eq!(debt.division_balance(1), IskCents::from_isk(2_700_000));
        assert_eq!(cred.master_wallet_balance(), IskCents::from_isk(1_150_000));
        assert_eq!(cred.division_balance(1), IskCents::from_isk(1_150_000));
    });
}

#[test]
fn test_408_market_buy_order_skill_range_and_immediate_fill_broker_fee() {
    let ctx = test_ctx();
    let buyer_id = 90_000_301_i64;
    let seller_id = 90_000_302_i64;
    ctx.db.with_mut_snapshot(|s| {
        ensure_test_char(s, buyer_id, "BuyerPilot", 1_000_035, JITA_STATION, 10_000_000);
        ensure_test_char(s, seller_id, "SellerPilot", 1_000_035, JITA_STATION, 100_000);
        s.market_orders.retain(|_, o| o.type_id != 34);
        s.market_orders.insert(950_001, MarketOrderRecord {
            order_id: 950_001, owner_id: seller_id, station_id: JITA_STATION, region_id: THE_FORGE,
            type_id: 34, is_bid: false, price: IskCents::from_isk(1_000), vol_entered: 60, vol_remaining: 60, min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
    });

    let mut buyer_sess = make_session(301, buyer_id, 1_000_035, JITA_STATION, 0);
    let kw = HashMap::new();

    // Buy order (duration=1) cannot bypass Visibility skill range check with range=32767
    let vis_err = crate::services::market_trading::handle(
        &ctx, &mut buyer_sess, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(10), PyRep::Integer(32_767), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    ).unwrap().unwrap_err();
    assert_eq!(vis_err.code, "VisibilitySkillRequired");

    // BuyMultipleItems enforces Visibility skill for solar-system range (0) and Procurement for remote station
    let bmulti_vis = crate::services::market_trading::handle(
        &ctx, &mut buyer_sess, "marketProxy", "BuyMultipleItems",
        &[PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(5)), ("orderRange", PyRep::Integer(0))])])],
        &kw,
    ).unwrap().unwrap_err();
    assert_eq!(bmulti_vis.code, "InsufficientVisibilitySkill");

    let remote_station = ctx
        .sde
        .stations
        .values()
        .find(|st| st.region_id == THE_FORGE && st.station_id != JITA_STATION)
        .map(|st| st.station_id)
        .unwrap_or(60_003_761);
    let bmulti_proc = crate::services::market_trading::handle(
        &ctx, &mut buyer_sess, "marketProxy", "BuyMultipleItems",
        &[PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(5)), ("stationID", PyRep::Integer(remote_station))])])],
        &kw,
    ).unwrap().unwrap_err();
    assert_eq!(bmulti_proc.code, "InsufficientProcurementSkill");

    // Partial immediate fill (60 immediate @ 1,000 ISK + 40 resting @ 1,000 ISK):
    // broker fee is charged ONLY on the resting 40,000 ISK (= 1,200.0 ISK at 3%),
    // NOT on all 100,000 ISK including immediately-filled units (= 3,000.0 ISK).
    let partial = crate::services::market_trading::handle(
        &ctx, &mut buyer_sess, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(100), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)],
        &kw,
    ).unwrap().unwrap();
    assert_eq!(partial.dict_get("brokerFeeISK").and_then(PyRep::as_f64), Some(1_200.0));
}

#[test]
fn test_409_invention_install_job_enforces_exact_sde_datacores_and_input_container() {
    let mut ctx = test_ctx();
    let (bp_type_id, req_mats, req_skills) = match ctx.sde.blueprints.values().find_map(|bp| {
        bp.activities.get("invention").and_then(|data| {
            if !data.materials.is_empty()
                && data.materials.iter().all(|m| m.type_id != 20424 && m.type_id != 20172 && m.quantity > 0)
            {
                Some((bp.blueprint_type_id, data.materials.clone(), data.skills.clone()))
            } else {
                None
            }
        })
    }) {
        Some(triple) => triple,
        None => {
            let sde = Arc::make_mut(&mut ctx.sde);
            let tid = 99_901_u32;
            let mats = vec![
                eve_sde::BlueprintMaterial { type_id: 20171, quantity: 2 },
                eve_sde::BlueprintMaterial { type_id: 20416, quantity: 2 },
            ];
            let mut activities = HashMap::new();
            activities.insert(
                "invention".to_string(),
                eve_sde::BlueprintActivity {
                    time: 600,
                    materials: mats.clone(),
                    products: vec![eve_sde::BlueprintProduct { type_id: 11568, quantity: 1, probability: Some(0.3) }],
                    skills: Vec::new(),
                },
            );
            sde.blueprints.insert(
                tid,
                eve_sde::BlueprintInfo {
                    blueprint_type_id: tid,
                    product_type_id: 11568,
                    product_quantity: 1,
                    manufacturing_time_sec: 3600,
                    max_production_limit: 10,
                    research_me_time_sec: 600,
                    research_te_time_sec: 600,
                    copy_time_sec: 600,
                    invention_time_sec: 600,
                    invention_base_probability: 0.3,
                    materials: Vec::new(),
                    activities,
                },
            );
            (tid, mats, Vec::new())
        }
    };

    let char_id = 90_000_401_i64;
    let bpc_id = 700_401_i64;
    let cont_id = 700_402_i64;
    let wrong_cont_id = 700_403_i64;
    let fac_id = ctx.sde.stations.values().find(|st| st.station_id == JITA_STATION).map(|st| st.station_id).unwrap_or(JITA_STATION);

    ctx.db.with_mut_snapshot(|s| {
        ensure_test_char(s, char_id, "InventorPilot", 1_000_035, fac_id, 50_000_000);
        let skills = s.skills.entry(char_id).or_default();
        for tid in [3380_u32, 11448, 11452, 21791] {
            skills.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
        for sk in &req_skills {
            skills.insert(sk.type_id, TrainedSkillRecord { type_id: sk.type_id, level: sk.level.max(1), skill_points: 256_000.0 });
        }
        s.items.insert(cont_id, InventoryItemRecord { item_id: cont_id, type_id: 3293, owner_id: char_id, location_id: fac_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
        s.items.insert(wrong_cont_id, InventoryItemRecord { item_id: wrong_cont_id, type_id: 3293, owner_id: char_id, location_id: fac_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
        s.items.insert(bpc_id, InventoryItemRecord { item_id: bpc_id, type_id: bp_type_id, owner_id: char_id, location_id: fac_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, is_blueprint_copy: true, blueprint_runs_remaining: Some(10), version: 1, ..Default::default() });
        // Give generic datacores 20424/20172 (must NOT bypass exact SDE datacores!)
        s.items.insert(700_410, InventoryItemRecord { item_id: 700_410, type_id: 20424, owner_id: char_id, location_id: cont_id, flag: InventoryFlag::Hangar, quantity: 50, version: 1, ..Default::default() });
        s.items.insert(700_411, InventoryItemRecord { item_id: 700_411, type_id: 20172, owner_id: char_id, location_id: cont_id, flag: InventoryFlag::Hangar, quantity: 50, version: 1, ..Default::default() });
    });

    let mut sess = make_session(401, char_id, 1_000_035, fac_id, 0);
    let kw = HashMap::new();
    let job_req = PyRep::keyval([
        ("facilityID", PyRep::Integer(fac_id)),
        ("blueprintID", PyRep::Integer(bpc_id)),
        ("activityID", PyRep::Integer(8)),
        ("runs", PyRep::Integer(1)),
        ("inputLocationID", PyRep::Integer(cont_id)),
    ]);

    // Bypass attempt with substitute datacores 20424/20172 must fail
    let err = crate::services::industry_pi::industry_jobs::handle_industry_jobs(
        &ctx, &mut sess, "industryManager", "InstallJob", std::slice::from_ref(&job_req), &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err.code, "InsufficientInventionDatacores");

    // Put exact SDE datacores in wrong_cont_id: must still fail when inputLocationID is cont_id
    ctx.db.with_mut_snapshot(|s| {
        for (idx, m) in req_mats.iter().enumerate() {
            s.items.insert(700_450 + idx as i64, InventoryItemRecord { item_id: 700_450 + idx as i64, type_id: m.type_id, owner_id: char_id, location_id: wrong_cont_id, flag: InventoryFlag::Hangar, quantity: m.quantity as i32, version: 1, ..Default::default() });
        }
    });
    let err_wrong_loc = crate::services::industry_pi::industry_jobs::handle_industry_jobs(
        &ctx, &mut sess, "industryManager", "InstallJob", std::slice::from_ref(&job_req), &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err_wrong_loc.code, "InsufficientInventionDatacores");

    // Move exact SDE datacores into requested inputLocationID cont_id: succeeds & consumes exact materials
    ctx.db.with_mut_snapshot(|s| {
        for idx in 0..req_mats.len() {
            if let Some(it) = s.items.get_mut(&(700_450 + idx as i64)) {
                it.location_id = cont_id;
            }
        }
    });
    let ok = crate::services::industry_pi::industry_jobs::handle_industry_jobs(
        &ctx, &mut sess, "industryManager", "InstallJob", std::slice::from_ref(&job_req), &kw,
    ).unwrap().unwrap();
    assert!(ok.dict_get("jobID").is_some());
    ctx.db.with_snapshot(|s| {
        for idx in 0..req_mats.len() {
            assert!(!s.items.contains_key(&(700_450 + idx as i64)));
        }
    });
}
