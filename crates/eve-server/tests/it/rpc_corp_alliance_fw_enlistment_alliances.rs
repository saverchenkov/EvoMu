//! Parity integration tests for `corpmgr` and `allianceRegistry` services (`SO-3`).
//!
//! Covers:
//! - `corpmgr` (11 rows: `AuditMember`, `GetAggressionSettings`, `GetAggressionSettingsForCorps`, `GetAssetInventory`, `GetAssetInventoryForLocation`, `GetCorporationIDForCharacter`, `GetCorporations`, `GetPublicInfo`, `SearchAssets`, `allianceID`, `ceoID`, `corporationID`)
//! - `allianceRegistry` (13 rows: `GetAlliance`, `GetAllianceMembers`, `GetAllianceMembersOlderThan`, `GetAlliancePublicInfo`, `GetDaysInAlliance`, `GetEmploymentRecord`, `GetRankedAlliances`, `allianceID`, `allowWar`, `creatorCharID`, `creatorCorpID`, `executorCorpID`, `shortName`)

use crate::common;
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn call(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    m: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, args, &HashMap::new(), None)
        .await
}

fn rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        other => panic!("expected Rowset, got {other:?}"),
    }
}

fn keyval(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.clone(),
        other => panic!("expected KeyVal, got {other:?}"),
    }
}

#[tokio::test]
async fn test_corpmgr_attributes_and_queries() {
    // parity: rpc:corpmgr.AuditMember, rpc:corpmgr.GetAggressionSettings, rpc:corpmgr.GetAggressionSettingsForCorps, rpc:corpmgr.GetAssetInventory, rpc:corpmgr.GetAssetInventoryForLocation, rpc:corpmgr.GetCorporationIDForCharacter, rpc:corpmgr.GetCorporations, rpc:corpmgr.GetPublicInfo, rpc:corpmgr.SearchAssets, rpc:corpmgr.allianceID, rpc:corpmgr.ceoID, rpc:corpmgr.corporationID
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2201);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    let mut sess2 = SessionState::new(2202);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("SelectCharacterID 90000002");

    // 1. GetCorporationIDForCharacter
    let cid1 = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetCorporationIDForCharacter",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetCorporationIDForCharacter 90000001");
    assert_eq!(cid1, PyRep::Integer(1_000_170));

    // 2. Create player corporation "Aegis Frontier Syndicate"
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Aegis Frontier Syndicate".into()),
            PyRep::String("AEGFS".into()),
            PyRep::Real(0.06),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = sess1.corporation_id.expect("player corp ID");

    sess2.corporation_id = Some(corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(90_000_002);
        }
    });

    // 3. GetPublicInfo, GetCorporations, and corpmgr attributes
    let pub_info = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetPublicInfo");
    let pub_kv = keyval(&pub_info);
    assert_eq!(pub_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));

    let get_corps = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetCorporations",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorporations");
    let corps_kv = keyval(&get_corps);
    assert_eq!(corps_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));

    assert_eq!(
        call(&ctx, &mut sess1, "corpmgr", "corporationID", &[])
            .await
            .expect("corporationID"),
        PyRep::Integer(corp_id)
    );
    assert_eq!(
        call(&ctx, &mut sess1, "corpmgr", "ceoID", &[])
            .await
            .expect("ceoID"),
        PyRep::Integer(90_000_001)
    );
    assert_eq!(
        call(&ctx, &mut sess1, "corpmgr", "allianceID", &[])
            .await
            .expect("allianceID"),
        PyRep::None
    );

    // 4. GetAggressionSettings (single KeyVal) & GetAggressionSettingsForCorps (Dict of corpID -> KeyVal)
    let single_agg = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("corpmgr.GetAggressionSettings");
    let single_agg_kv = keyval(&single_agg);
    assert_eq!(
        single_agg_kv.get("isFriendlyFireLegal"),
        Some(&PyRep::Bool(false))
    );

    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAggressionSettings");
    ctx.advance_secs(86_400);

    let agg_for_corps = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAggressionSettingsForCorps",
        &[PyRep::List(vec![
            PyRep::Integer(corp_id),
            PyRep::Integer(98_999_999),
        ])],
    )
    .await
    .expect("GetAggressionSettingsForCorps list");
    let PyRep::Dict(agg_map) = agg_for_corps else {
        panic!("expected PyRep::Dict from GetAggressionSettingsForCorps");
    };
    assert_eq!(agg_map.len(), 2);
    let (_, c1_agg) = agg_map
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(corp_id))
        .expect("corp_id entry");
    assert_eq!(
        keyval(c1_agg).get("isFriendlyFireLegal"),
        Some(&PyRep::Bool(true))
    );
    let (_, c2_agg) = agg_map
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(98_999_999))
        .expect("unconfigured corp entry");
    assert_eq!(
        keyval(c2_agg).get("isFriendlyFireLegal"),
        Some(&PyRep::Bool(false))
    );

    // 5. GetAssetInventory, GetAssetInventoryForLocation, and SearchAssets (including rented office folder resolution)
    ctx.db.with_mut_snapshot(|snap| {
        // Rented office folder (type 27) at station 60_003_760
        snap.items.insert(
            200_000_100,
            InventoryItemRecord {
                item_id: 200_000_100,
                type_id: 27,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Two items stored inside the office folder (location_id = 200_000_100)
        snap.items.insert(
            200_000_101,
            InventoryItemRecord {
                item_id: 200_000_101,
                type_id: 587, // Rifter
                owner_id: corp_id,
                location_id: 200_000_100,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            200_000_102,
            InventoryItemRecord {
                item_id: 200_000_102,
                type_id: 34, // Tritanium
                owner_id: corp_id,
                location_id: 200_000_100,
                flag: InventoryFlag::CorpSAG2,
                quantity: 5_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Market delivery item at station 60_003_760
        snap.items.insert(
            200_000_103,
            InventoryItemRecord {
                item_id: 200_000_103,
                type_id: 35, // Pyerite
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::CorpDeliveries,
                quantity: 1_200,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // GetAssetInventory deduplicates by station and resolves officeFolderID -> stationID
    let inv_offices = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAssetInventory",
        &[PyRep::Integer(corp_id), PyRep::String("offices".into())],
    )
    .await
    .expect("GetAssetInventory offices");
    let (_, inv_rows) = rows(&inv_offices);
    assert_eq!(inv_rows.len(), 1, "GetAssetInventory deduplicates to 1 row per station");
    assert_eq!(inv_rows[0][3], PyRep::Integer(60_003_760));

    // GetAssetInventoryForLocation returns both items in the office at 60_003_760
    let assets_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAssetInventoryForLocation",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(60_003_760),
            PyRep::String("offices".into()),
        ],
    )
    .await
    .expect("GetAssetInventoryForLocation offices");
    let (_, ast_rows) = rows(&assets_rep);
    assert_eq!(ast_rows.len(), 2);

    let deliv_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAssetInventoryForLocation",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(60_003_760),
            PyRep::String("deliveries".into()),
        ],
    )
    .await
    .expect("GetAssetInventoryForLocation deliveries");
    let (_, deliv_rows) = rows(&deliv_rep);
    assert_eq!(deliv_rows.len(), 1);
    assert_eq!(deliv_rows[0][0], PyRep::Integer(200_000_103));

    // SearchAssets (6-arg V24 with leading corp_id and minQty filter)
    let search_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "SearchAssets",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("offices".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(34),
            PyRep::Integer(1_000),
        ],
    )
    .await
    .expect("SearchAssets 6-arg");
    let (_, search_rows) = rows(&search_rep);
    assert_eq!(search_rows.len(), 1);
    assert_eq!(search_rows[0][0], PyRep::Integer(200_000_102));

    // Regular member without director/accountant/trader role is denied
    let denied_assets = call(
        &ctx,
        &mut sess2,
        "corpmgr",
        "GetAssetInventoryForLocation",
        &[PyRep::Integer(60_003_760)],
    )
    .await
    .expect_err("Non-director member denied asset inventory");
    assert_eq!(denied_assets.code, "CorpRoleDenied");

    // 6. AuditMember
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64),
        ],
    )
    .await
    .expect("UpdateMember role grant");

    let audit_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("AuditMember");
    let (_, aud_rows) = rows(&audit_rep);
    assert_eq!(aud_rows.len(), 1);
}

#[tokio::test]
async fn test_alliance_registry_queries_and_attributes() {
    // parity: rpc:allianceRegistry.GetAlliance, rpc:allianceRegistry.GetAllianceMembers, rpc:allianceRegistry.GetAllianceMembersOlderThan, rpc:allianceRegistry.GetAlliancePublicInfo, rpc:allianceRegistry.GetDaysInAlliance, rpc:allianceRegistry.GetEmploymentRecord, rpc:allianceRegistry.GetRankedAlliances, rpc:allianceRegistry.allianceID, rpc:allianceRegistry.allowWar, rpc:allianceRegistry.creatorCharID, rpc:allianceRegistry.creatorCorpID, rpc:allianceRegistry.executorCorpID, rpc:allianceRegistry.shortName
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2301);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    let mut sess2 = SessionState::new(2302);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("SelectCharacterID 90000002");

    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Imperial Zenith Holdings".into()),
            PyRep::String("IZH".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 1");
    let corp1 = sess1.corporation_id.expect("corp1 ID");

    call(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Solaris Apex Fleet".into()),
            PyRep::String("SOLAF".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 2");
    let corp2 = sess2.corporation_id.expect("corp2 ID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp1) {
            c.ensure_wallet_divisions();
            c.wallet_divisions[0].balance = IskCents::from_isk(2_500_000_000);
            c.master_wallet = c.wallet_divisions[0].balance;
        }
    });

    let create_rep = call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Zenith Apex Coalition".into()),
            PyRep::String("ZAC".into()),
            PyRep::String("Premier nullsec coalition".into()),
            PyRep::String("https://zenith.eve".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let create_kv = keyval(&create_rep);
    let aid = match create_kv.get("allianceID") {
        Some(PyRep::Integer(id)) => *id,
        other => panic!("expected integer allianceID, got {other:?}"),
    };
    assert_eq!(create_kv.get("executorCorpID"), Some(&PyRep::Integer(corp1)));
    assert_eq!(sess1.alliance_id, Some(aid));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.alliances.get_mut(&aid) {
            a.member_corp_ids.insert(corp2);
        }
        if let Some(c) = snap.corporations.get_mut(&corp2) {
            c.alliance_id = Some(aid);
        }
    });

    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "allianceID", &[])
            .await
            .expect("allianceID"),
        PyRep::Integer(aid)
    );
    // rpc-lint: allow ticker literal
    let expected_short = "ZAC";
    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "shortName", &[])
            .await
            .expect("shortName"),
        PyRep::String(expected_short.into())
    );
    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "executorCorpID", &[])
            .await
            .expect("executorCorpID"),
        PyRep::Integer(corp1)
    );
    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "creatorCorpID", &[])
            .await
            .expect("creatorCorpID"),
        PyRep::Integer(corp1)
    );
    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "creatorCharID", &[])
            .await
            .expect("creatorCharID"),
        PyRep::Integer(90_000_001)
    );
    assert_eq!(
        call(&ctx, &mut sess1, "allianceRegistry", "allowWar", &[])
            .await
            .expect("allowWar"),
        PyRep::Bool(true)
    );

    let all_info = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAlliance");
    let all_kv = keyval(&all_info);
    assert_eq!(all_kv.get("allianceID"), Some(&PyRep::Integer(aid)));
    assert_eq!(all_kv.get("memberCount"), Some(&PyRep::Integer(2)));

    let pub_all_info = call(
        &ctx,
        &mut sess2,
        "allianceRegistry",
        "GetAlliancePublicInfo",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAlliancePublicInfo");
    assert_eq!(keyval(&pub_all_info).get("memberCount"), Some(&PyRep::Integer(2)));

    let members_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAllianceMembers",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAllianceMembers");
    let (_, m_rows) = rows(&members_rep);
    assert_eq!(m_rows.len(), 2);

    let older_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAllianceMembersOlderThan",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAllianceMembersOlderThan");
    let (_, older_rows) = rows(&older_rep);
    assert_eq!(older_rows.len(), 2);

    let emp_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetEmploymentRecord",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetEmploymentRecord");
    let (_, e_rows) = rows(&emp_rep);
    assert_eq!(e_rows.len(), 1);

    let days_0 = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetDaysInAlliance",
        &[PyRep::Integer(corp1)],
    )
    .await
    .expect("GetDaysInAlliance initial");
    assert_eq!(days_0, PyRep::Integer(0));

    ctx.advance_secs(432_000);
    let days_5 = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetDaysInAlliance",
        &[PyRep::Integer(corp1)],
    )
    .await
    .expect("GetDaysInAlliance after 5 days");
    assert_eq!(days_5, PyRep::Integer(5));

    let ranked_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetRankedAlliances",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("GetRankedAlliances");
    let (r_cols, r_rows) = rows(&ranked_rep);
    assert!(r_cols.contains(&"creatorCorpID".to_string()));
    assert!(r_cols.contains(&"creatorID".to_string()));
    assert_eq!(r_rows.len(), 1);
    assert_eq!(r_rows[0][0], PyRep::Integer(aid));
    assert_eq!(r_rows[0][4], PyRep::Integer(2));
}
