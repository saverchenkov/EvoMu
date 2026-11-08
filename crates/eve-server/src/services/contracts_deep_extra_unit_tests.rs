//! Deep branch-coverage & mutation-killing unit tests for `contracts_service`.

use crate::router::ServerContext;
use crate::services::contracts_service::handle;
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) const JITA_STATION_ID: i64 = 60_003_760;
pub(super) const DEST_STRUCT_ID: i64 = 1_000_000_910_002;
pub(super) const JITA_SYSTEM_ID: u32 = 30_000_142;
pub(super) const THE_FORGE_REGION_ID: u32 = 10_000_002;

pub(super) fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            DEST_STRUCT_ID,
            UpwellStructureRecord {
                structure_id: DEST_STRUCT_ID,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Dest Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
    });
    ctx
}

pub(super) fn make_authed_session(
    user_id: u64, char_id: i64, corp_id: i64, alliance_id: Option<i64>, station_id: Option<i64>, ship_id: Option<i64>,
) -> SessionState {
    SessionState {
        session_id: user_id, user_id: Some(user_id), character_id: Some(char_id), corporation_id: Some(corp_id),
        alliance_id, station_id, solar_system_id: Some(JITA_SYSTEM_ID), constellation_id: Some(20_000_020),
        region_id: Some(THE_FORGE_REGION_ID), ship_id, ..Default::default()
    }
}

pub(super) fn seed_char(ctx: &ServerContext, char_id: i64, corp_id: i64, alliance_id: Option<i64>, station_id: Option<i64>, ship_id: i64, isk: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("seeded char").clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(isk);
        ch.security_status = 0.0;
        snap.characters.insert(char_id, ch);
    });
}

pub(super) fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default()
            },
        );
    });
}

#[test]
fn create_contract_validation_and_for_corp_role_and_slot_branches() {
    let ctx = make_test_context();
    let issuer_id = 910_001;
    let corp_id = 98_000_001;
    let other_ceo = 910_099;
    seed_char(&ctx, issuer_id, corp_id, None, Some(JITA_STATION_ID), 910_500, 100_000_000);
    seed_item(&ctx, 910_100, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 1_000, false);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Contract Test Corp".into(),
            ticker: "CTST".into(),
            ceo_id: other_ceo,
            master_wallet: IskCents::from_isk(50_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(50_000_000);
        }
        snap.corporations.insert(corp_id, corp);
    });

    let mut sess = make_authed_session(1, issuer_id, corp_id, None, Some(JITA_STATION_ID), Some(910_500));
    let kw = HashMap::new();

    // 1. Invalid destination station ID on Courier -> InvalidContractStation
    let err = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(30_000_142)),
            ("itemList", PyRep::List(vec![PyRep::Integer(910_100)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidContractStation");

    // 2. Courier contract without items -> CourierContractRequiresItems
    let err = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(DEST_STRUCT_ID)),
            ("reward", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CourierContractRequiresItems");

    // 3. Invalid requestedItems format (not a list/tuple) and qty <= 0 -> InvalidContractParameters
    for bad_req in [
        PyRep::Integer(42),
        PyRep::List(vec![PyRep::keyval([
            ("typeID", PyRep::Integer(35)),
            ("quantity", PyRep::Integer(0)),
        ])]),
    ] {
        let err = handle(
            &ctx,
            &mut sess,
            "contractProxy",
            "CreateContract",
            &[PyRep::keyval([
                ("type", PyRep::String("ItemExchange".into())),
                ("startStationID", PyRep::Integer(JITA_STATION_ID)),
                ("itemList", PyRep::List(vec![PyRep::Integer(910_100)])),
                ("requestedItems", bad_req),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidContractParameters");
    }

    // 4. forCorp without ROLE_CONTRACT_MANAGER / ROLE_DIRECTOR / CEO -> CrpAccessDenied
    let req_kv = PyRep::List(vec![PyRep::keyval([
        ("typeID", PyRep::Integer(35)),
        ("quantity", PyRep::Integer(250)),
    ])]);
    let err = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(100_000.0)),
            ("requestedItems", req_kv.clone()),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // Grant ROLE_CONTRACT_MANAGER and verify Corporation Contracting (25233) slot cap (10 + 10 * lvl)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles
                .insert(issuer_id, eve_db::corp_roles::ROLE_CONTRACT_MANAGER);
        }
        snap.skills.entry(issuer_id).or_default().insert(
            25233,
            TrainedSkillRecord { type_id: 25233, level: 0, skill_points: 0.0 },
        );
        for idx in 0..10 {
            let cid = 800_000 + idx;
            snap.contracts.insert(
                cid,
                ContractRecord {
                    contract_id: cid,
                    issuer_id,
                    issuer_corp_id: corp_id,
                    for_corp: true,
                    status: "Outstanding".into(),
                    contract_type: "ItemExchange".into(),
                    start_station_id: JITA_STATION_ID,
                    end_station_id: JITA_STATION_ID,
                    expiration_unix: u64::MAX,
                    ..Default::default()
                },
            );
        }
    });

    let err = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(100_000.0)),
            ("requestedItems", req_kv.clone()),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "ContractSlotLimitReached");

    // Train Corporation Contracting (25233) to level 1 (allows 20 slots), funded forCorp without ROLE_ACCOUNT_CAN_TAKE[0] -> CrpAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(issuer_id).or_default().insert(
            25233,
            TrainedSkillRecord { type_id: 25233, level: 1, skill_points: 500.0 },
        );
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.set_division_balance(1, IskCents(0));
        }
    });
    let err_no_acct = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("availability", PyRep::Integer(0)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(1_000_000.0)),
            ("requestedItems", req_kv.clone()),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_acct.code, "CrpAccessDenied");

    // Grant ROLE_ACCOUNT_CAN_TAKE[0]; with empty corp division 1 wallet -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(
                issuer_id,
                eve_db::corp_roles::ROLE_CONTRACT_MANAGER | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
            );
        }
    });
    let err = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("availability", PyRep::Integer(0)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(1_000_000.0)),
            ("requestedItems", req_kv.clone()),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");

    // Fund corp division 1 and create a corporate ItemExchange contract with KeyVal requestedItems
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.set_division_balance(1, IskCents::from_isk(50_000_000));
        }
    });
    let created = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("availability", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(1_000_000.0)),
            ("requestedItems", req_kv),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        let rec = snap.contracts.get(&cid).unwrap();
        assert!(rec.for_corp);
        assert_eq!(rec.availability, "Corporation");
        assert_eq!(rec.requested_items, vec![(35, 250)]);
        assert!(!snap.service_state_blobs.contains_key(&format!("contract_deposit:{cid}")));
    });
}
