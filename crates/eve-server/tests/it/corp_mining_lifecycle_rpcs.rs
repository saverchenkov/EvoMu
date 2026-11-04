//! End-to-end integration tests for Batch 22 open `feat:contracts.*` and `feat:corporation.*` rows.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

pub(crate) const PILOT_1: i64 = 90_000_001;
pub(crate) const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;

pub(crate) async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None)
        .await
}

pub(crate) fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone())
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

pub(crate) fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        PyRep::List(items) => items
            .iter()
            .map(|it| match it {
                PyRep::List(r) | PyRep::Tuple(r) => r.clone(),
                other => vec![other.clone()],
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn test_b22_contracts_blueprints_and_plastic_wrap_break() {
    // parity: feat:contracts.blueprint-singleton-preservation, feat:contracts.courier-plastic-wrap-break, feat:contracts.item-exchange-blueprints-metadata
    eve_testkit::attest(&[
        "feat:contracts.blueprint-singleton-preservation",
        "feat:contracts.courier-plastic-wrap-break",
        "feat:contracts.item-exchange-blueprints-metadata",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 72_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 72_002, "other", "other-secret", PILOT_2).await;

    let (bpo_id, bpc_id, abyssal_id, courier_cargo_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
        s.next_entity_id += 4;
        let base = s.next_entity_id;

        let bpo_attrs = HashMap::from([(1112, 10.0), (1113, 20.0)]);
        s.items.insert(base - 3, InventoryItemRecord {
            item_id: base - 3, type_id: 691, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, dynamic_attributes: bpo_attrs, ..Default::default()
        });
        let bpc_attrs = HashMap::from([(1112, 2.0), (1113, 4.0)]);
        s.items.insert(base - 2, InventoryItemRecord {
            item_id: base - 2, type_id: 691, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, is_blueprint_copy: true,
            blueprint_runs_remaining: Some(15), dynamic_attributes: bpc_attrs, ..Default::default()
        });
        let ab_attrs = HashMap::from([(3, 12.5), (204, 1.35)]);
        s.items.insert(base - 1, InventoryItemRecord {
            item_id: base - 1, type_id: 5975, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true,
            source_mutaplasmid_id: Some(47746), dynamic_attributes: ab_attrs, ..Default::default()
        });
        s.items.insert(base, InventoryItemRecord {
            item_id: base, type_id: 34, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 500, ..Default::default()
        });
        (base - 3, base - 2, base - 1, base)
    });

    // 1. Create ItemExchange contract with BPO, BPC, and abyssal module (V24 1-arg KeyVal + confirm kwarg)
    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    let ie_params = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(7)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(1_000_000.0)),
        ("reward", PyRep::Real(0.0)),
        ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("BPO + BPC + Abyssal".into())),
        (
            "itemList",
            PyRep::List(vec![
                PyRep::Integer(bpo_id),
                PyRep::Integer(bpc_id),
                PyRep::Integer(abyssal_id),
            ]),
        ),
    ]);
    let create_ie = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[ie_params], &confirm_kw)
        .await
        .expect("CreateContract ItemExchange");
    let ie_cid = field(&create_ie, "contractID").as_i64().unwrap_or_else(|| create_ie.as_i64().unwrap());

    // Inspect via contractProxy.GetContract & contractProxy.SearchContracts
    let info = rpc(&ctx, &mut sess2, "contractProxy", "GetContract", &[PyRep::Integer(ie_cid)])
        .await
        .expect("contractProxy.GetContract");
    let PyRep::List(items) = field(&info, "items") else {
        panic!("expected items list in GetContractInfo");
    };
    assert_eq!(items.len(), 3);
    let bpc_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(bpc_id)).expect("bpc_entry");
    assert_eq!(field(bpc_entry, "isBlueprintCopy"), PyRep::Bool(true));
    assert_eq!(field(bpc_entry, "materialEfficiency"), PyRep::Integer(2));
    assert_eq!(field(bpc_entry, "timeEfficiency"), PyRep::Integer(4));
    assert_eq!(field(bpc_entry, "runsRemaining"), PyRep::Integer(15));

    let ab_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(abyssal_id)).expect("ab_entry");
    assert_eq!(field(ab_entry, "sourceMutaplasmidID"), PyRep::Integer(47746));
    assert_eq!(field(ab_entry, "damage"), PyRep::Real(12.5));

    let search_kw = HashMap::from([("contractType".to_string(), PyRep::String("ItemExchange".into()))]);
    let search = rpc_kw(&ctx, &mut sess2, "contractProxy", "SearchContracts", &[], &search_kw)
        .await
        .expect("contractProxy.SearchContracts");
    let PyRep::List(found_contracts) = search else {
        panic!("expected list from SearchContracts");
    };
    let found_ie = found_contracts
        .iter()
        .find(|c| field(c, "contractID") == PyRep::Integer(ie_cid))
        .expect("ItemExchange contract in SearchContracts");
    assert_eq!(field(found_ie, "title"), PyRep::String("BPO + BPC + Abyssal".into()));
    assert_eq!(field(found_ie, "type"), PyRep::String("ItemExchange".into()));

    // Accept contract (V24 2-arg: contractID, forCorp) and verify singleton metadata preserved
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(ie_cid), PyRep::Bool(false)])
        .await
        .expect("AcceptContract ItemExchange");

    ctx.db.with_snapshot(|s| {
        let bpo = s.items.get(&bpo_id).unwrap();
        assert_eq!(bpo.owner_id, PILOT_2);
        assert!(!bpo.is_blueprint_copy);
        assert_eq!(bpo.dynamic_attributes.get(&1112).copied(), Some(10.0));
        assert_eq!(bpo.dynamic_attributes.get(&1113).copied(), Some(20.0));

        let bpc = s.items.get(&bpc_id).unwrap();
        assert_eq!(bpc.owner_id, PILOT_2);
        assert!(bpc.is_blueprint_copy);
        assert_eq!(bpc.blueprint_runs_remaining, Some(15));

        let ab = s.items.get(&abyssal_id).unwrap();
        assert_eq!(ab.owner_id, PILOT_2);
        assert_eq!(ab.source_mutaplasmid_id, Some(47746));
        assert_eq!(ab.dynamic_attributes.get(&204).copied(), Some(1.35));
    });

    // 2. Courier plastic wrap break forfeits collateral (V24 1-arg KeyVal + confirm kwarg)
    let courier_params = PyRep::keyval([
        ("contractType", PyRep::String("Courier".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(0.0)),
        ("reward", PyRep::Real(250_000.0)),
        ("collateral", PyRep::Real(2_000_000.0)),
        ("title", PyRep::String("Courier Wrap Test".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(courier_cargo_id)])),
    ]);
    let create_courier = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[courier_params], &confirm_kw)
        .await
        .expect("CreateContract Courier");
    let courier_cid = field(&create_courier, "contractID").as_i64().unwrap_or_else(|| create_courier.as_i64().unwrap());
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(courier_cid), PyRep::Bool(false)])
        .await
        .expect("AcceptContract Courier");

    let issuer_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());

    let wrap_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| it.type_id == 3468 && it.owner_id == PILOT_2)
            .map(|it| it.item_id)
            .expect("Courier Plastic Wrap 3468 in hauler hangar")
    });

    // Repackage (break) the plastic wrap (V24 1-arg + destroyable kwarg)
    let destroy_kw = HashMap::from([("destroyable".to_string(), PyRep::Bool(true))]);
    rpc_kw(
        &ctx,
        &mut sess2,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(wrap_id)])],
        &destroy_kw,
    )
    .await
    .expect("RepackageItems plastic wrap");

    ctx.db.with_snapshot(|s| {
        let c = s.contracts.get(&courier_cid).unwrap();
        assert_eq!(c.status, "Failed", "Courier contract must immediately fail on plastic wrap break");
        let cargo = s.items.get(&courier_cargo_id).unwrap();
        assert_eq!(cargo.owner_id, PILOT_2);
        assert_eq!(cargo.location_id, JITA_STATION);
        assert_eq!(cargo.flag, InventoryFlag::Hangar);
        assert!(!s.items.contains_key(&wrap_id), "Plastic wrap must be destroyed");
        let issuer_bal_after = s.characters[&PILOT_1].balance.raw_cents();
        assert_eq!(issuer_bal_after - issuer_bal_before, 2_250_000_00, "Collateral + escrowed reward must transfer/refund to issuer");
    });
}

#[tokio::test]
async fn test_b22_corp_settings_mail_and_structure_defaults() {
    // parity: feat:corporation.accept-structures-setting, feat:corporation.corp-mail-restriction, feat:corporation.structure-reinforce-default, feat:corporation.welcome-mail
    eve_testkit::attest(&[
        "feat:corporation.accept-structures-setting",
        "feat:corporation.corp-mail-restriction",
        "feat:corporation.structure-reinforce-default",
        "feat:corporation.welcome-mail",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 72_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 72_102, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Vanguard Holdings".into()),
            PyRep::String("VNGD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().unwrap();

    // 1. Welcome mail setting (V24 1-arg SetCorpWelcomeMail + None when unset) + automatic delivery on join
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "GetCorpWelcomeMail", &[])
            .await
            .expect("GetCorpWelcomeMail unset"),
        PyRep::None,
        "GetCorpWelcomeMail must return None when unset"
    );
    let deny_wm = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[PyRep::String("Unauthorized".into())],
    )
    .await;
    assert!(deny_wm.is_err(), "Non-member cannot set corp welcome mail");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[PyRep::String("Read the corp bulletin!".into())],
    )
    .await
    .expect("SetCorpWelcomeMail");

    let wm = rpc(&ctx, &mut sess1, "corpRegistry", "GetCorpWelcomeMail", &[])
        .await
        .expect("GetCorpWelcomeMail");
    assert_eq!(field(&wm, "body"), PyRep::String("Read the corp bulletin!".into()));

    let app_rep = rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining!".into())])
        .await
        .expect("InsertApplication");
    let app_id = field(&app_rep, "applicationID").as_i64().unwrap_or_else(|| app_rep.as_i64().unwrap());

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Accepted".into()),
            PyRep::Integer(6),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ],
    )
    .await
    .expect("UpdateApplicationOffer");

    ctx.db.with_snapshot(|s| {
        assert!(
            s.mails.iter().any(|m| m.recipient_id == PILOT_2 && m.body == "Read the corp bulletin!"),
            "New member must automatically receive corp welcome mail on joining"
        );
    });

    // 2. Corp mail restriction
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewCorpMailRestrictionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewCorpMailRestrictionSettings");
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "DoesCorpRestrictCorpMails", &[])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );

    let restricted_send = rpc(
        &ctx,
        &mut sess2,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(Vec::new()),
            PyRep::None,
            PyRep::Integer(corp_id),
            PyRep::String("Corp Spam".into()),
            PyRep::String("Hello everyone".into()),
            PyRep::None,
            PyRep::None,
        ],
    )
    .await;
    assert_eq!(
        restricted_send.unwrap_err().code,
        "CorpMailRestricted",
        "Non-authorised member must be blocked with CorpMailRestricted"
    );

    rpc(
        &ctx,
        &mut sess1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(Vec::new()),
            PyRep::None,
            PyRep::Integer(corp_id),
            PyRep::String("CEO Announcement".into()),
            PyRep::String("Important corp update".into()),
            PyRep::None,
            PyRep::None,
        ],
    )
    .await
    .expect("CEO must be allowed to send restricted corp mail");

    // 3. Accept structures setting + transfer rejection/acceptance
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings(false)");
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "DoesMyCorpAcceptStructures", &[])
            .await
            .unwrap(),
        PyRep::Bool(false)
    );

    // 4. Structure reinforce default: non-director role gate + adopted by newly deployed structure
    let deny_reinf = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(5)],
    )
    .await;
    assert_eq!(
        deny_reinf.unwrap_err().code,
        "CorpRoleDenied",
        "Non-director member must be rejected when setting structure reinforce default"
    );
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "GetStructureReinforceDefault", &[])
            .await
            .unwrap(),
        PyRep::Integer(18),
        "Default structure reinforce hour must remain unchanged after rejected call"
    );
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(21)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "GetStructureReinforceDefault", &[])
            .await
            .unwrap(),
        PyRep::Integer(21)
    );
    assert_eq!(
        rpc(&ctx, &mut sess2, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(3)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut sess1, "corpRegistry", "GetStructureReinforceDefault", &[])
            .await
            .unwrap(),
        PyRep::Integer(21)
    );

    let (sys_id, station_id, base_pos) = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == 30_000_144)
        .map(|s| (s.solar_system_id, s.station_id, s.position))
        .unwrap();
    let ship_id = sess1.ship_id.unwrap();
    sess1.station_id = Some(station_id);
    sess1.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&PILOT_1) {
            c.station_id = Some(station_id);
            c.solar_system_id = sys_id;
        }
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.location_id = station_id;
        }
        if let Some(gov) = s.corp_governance.get_mut(&corp_id) {
            gov.created_unix = gov.created_unix.saturating_sub(8 * 86_400);
        }
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: 35832, owner_id: PILOT_1, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
        });
    });
    rpc(&ctx, &mut sess1, "structureDocking", "Undock", &[])
        .await
        .expect("Undock");
    ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, base_pos + glam::DVec3::new(0.0, 5.0e9, 0.0)).await;
    let anchor_rep = rpc(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(35832), PyRep::String("Vanguard Astrahus".into())],
    )
    .await
    .expect("structureDeployment.Anchor");
    let st_id = field(&anchor_rep, "structureID").as_i64().unwrap();
    ctx.db.with_snapshot(|s| {
        let (hour, _) = s.structure_vulnerability_windows.get(&st_id).copied().expect("vuln window");
        assert_eq!(hour, 21, "Newly deployed corp structure must adopt corp default reinforce hour 21");
    });

    // Transfer to a corporation with accept_structures = false must be rejected
    let target_corp_id = 98_999_001;
    ctx.db.with_mut_snapshot(|s| {
        let mut target_corp = s.corporations.get(&corp_id).unwrap().clone();
        target_corp.corporation_id = target_corp_id;
        target_corp.name = "Refusing Corp".into();
        s.corporations.insert(target_corp_id, target_corp);
        let gov = s.corp_governance.entry(target_corp_id).or_default();
        gov.accept_structures_set = true;
        gov.accept_structures = false;
    });
    let reject_xfer = rpc(
        &ctx,
        &mut sess1,
        "beyonce",
        "CmdTransferCorpAssetOwnership",
        &[PyRep::Integer(st_id), PyRep::Integer(target_corp_id), PyRep::None],
    )
    .await;
    assert_eq!(
        reject_xfer.unwrap_err().code,
        "CorpDoesNotAcceptStructures",
        "Structure transfer to corp with accept_structures=false must be rejected"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&st_id].owner_corp_id, corp_id);
    });

    ctx.db.with_mut_snapshot(|s| {
        s.corp_governance.get_mut(&target_corp_id).unwrap().accept_structures = true;
    });
    rpc(
        &ctx,
        &mut sess1,
        "beyonce",
        "CmdTransferCorpAssetOwnership",
        &[PyRep::Integer(st_id), PyRep::Integer(target_corp_id), PyRep::None],
    )
    .await
    .expect("Structure transfer must succeed when target corp accepts structures");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&st_id].owner_corp_id, target_corp_id);
    });
}

pub(crate) mod corp_governance_shares_dividends_ceo {
    // End-to-end integration tests for Batch 22 corporation governance, projects, and NPC corp rules.


    use eve_db::IskCents;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    use crate::corp_mining_lifecycle_rpcs::{field, rowset_rows, rpc, select_pilot, PILOT_1, PILOT_2};

    #[tokio::test]
    async fn test_b22_corp_governance_shares_dividends_ceo_divisions_autokick() {
        // parity: feat:corporation.ceo-resign, feat:corporation.corp-division-names, feat:corporation.dividends, feat:corporation.share-transfer
        eve_testkit::attest(&[
            "feat:corporation.ceo-resign",
            "feat:corporation.corp-division-names",
            "feat:corporation.dividends",
            "feat:corporation.share-transfer",
        ]);

        let ctx = ServerContext::new();
        let mut sess1 = select_pilot(&ctx, 72_201, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = select_pilot(&ctx, 72_202, "other", "other-secret", PILOT_2).await;

        let created = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Sovereign Syndicate".into()),
                PyRep::String("SVRN".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation");
        let corp_id = field(&created, "corporationID").as_i64().unwrap();

        let dock_id = sess2.station_id.unwrap_or(60_003_760);
        let mut trit_item_id = 0_i64;
        ctx.db.with_mut_snapshot(|s| {
            let corp = s.corporations.get_mut(&corp_id).unwrap();
            corp.members.insert(PILOT_2);
            corp.ensure_wallet_divisions();
            corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
            corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
            s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
            s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
            s.next_entity_id += 1;
            trit_item_id = s.next_entity_id;
            s.items.insert(
                trit_item_id,
                eve_db::InventoryItemRecord {
                    item_id: trit_item_id,
                    type_id: 34,
                    owner_id: PILOT_2,
                    location_id: dock_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        });
        sess2.corporation_id = Some(corp_id);

        // 1. Division names (V24 client 14-argument call shape + GetDivisionNames + ESI divisions)
        let hangar_names = [
            "Armory", "Ore Bay", "Components", "Ammo", "Salvage", "Blueprints", "Capitals",
        ];
        let wallet_names = [
            "Treasury", "SRP", "Industry", "Fuel", "Taxes", "Bounties", "Reserve",
        ];
        let div_args_14: Vec<PyRep> = hangar_names
            .iter()
            .chain(wallet_names.iter())
            .map(|s| PyRep::String((*s).into()))
            .collect();
        let deny_div = rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "UpdateDivisionNames",
            &div_args_14,
        )
        .await;
        assert_eq!(deny_div.unwrap_err().code, "CorpRoleDenied", "Non-director cannot rename divisions");

        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "UpdateDivisionNames",
            &div_args_14,
        )
        .await
        .expect("UpdateDivisionNames (14 args)");
        let divs = rpc(&ctx, &mut sess1, "corpRegistry", "GetDivisionNames", &[PyRep::Integer(corp_id)])
            .await
            .expect("GetDivisionNames");
        for i in 0..7 {
            assert_eq!(field(&divs, &format!("division{}", i + 1)), PyRep::String(hangar_names[i].into()));
            assert_eq!(field(&divs, &format!("walletDivision{}", i + 1)), PyRep::String(wallet_names[i].into()));
        }
        let mut esi_params = HashMap::new();
        esi_params.insert("corporation_id".to_string(), corp_id.to_string());
        let esi_divs = eve_server::esi::sample_routes::dispatch_operation(
            &ctx,
            ctx.now_unix(),
            "GET",
            "/corporations/{corporation_id}/divisions",
            &esi_params,
            &HashMap::new(),
            None,
            1,
            &[],
        );
        assert_eq!(esi_divs.status_code, 200, "ESI divisions route failed: {esi_divs:?}");
        let esi_body = esi_divs.body.expect("ESI divisions JSON");
        assert_eq!(esi_body["hangar"][0]["name"], "Armory");
        assert_eq!(esi_body["hangar"][6]["name"], "Capitals");
        assert_eq!(esi_body["wallet"][0]["name"], "Treasury");
        assert_eq!(esi_body["wallet"][6]["name"], "Reserve");

        // 2. Share transfers (MoveCompanyShares + MovePrivateShares) & exact share counts
        let deny_shares = rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(100)],
        )
        .await;
        assert_eq!(deny_shares.unwrap_err().code, "CorpRoleDenied");

        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(400)],
        )
        .await
        .expect("MoveCompanyShares");

        let sh_p2 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_2)])
            .await
            .expect("GetSharesByShareholder PILOT_2");
        assert_eq!(
            rowset_rows(&sh_p2),
            vec![vec![PyRep::Integer(PILOT_2), PyRep::Integer(corp_id), PyRep::Integer(400)]]
        );

        rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "MovePrivateShares",
            &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_1), PyRep::Integer(150)],
        )
        .await
        .expect("MovePrivateShares");

        let sh_p2_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_2)])
            .await
            .expect("GetSharesByShareholder PILOT_2 after");
        assert_eq!(
            rowset_rows(&sh_p2_after),
            vec![vec![PyRep::Integer(PILOT_2), PyRep::Integer(corp_id), PyRep::Integer(250)]]
        );
        let sh_p1_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_1)])
            .await
            .expect("GetSharesByShareholder PILOT_1 after");
        assert_eq!(
            rowset_rows(&sh_p1_after),
            vec![vec![PyRep::Integer(PILOT_1), PyRep::Integer(corp_id), PyRep::Integer(150)]]
        );

        // 3. PayoutDividend (negative cases + exact amounts + wallet journals on both sides)
        let deny_div_pay = rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "PayoutDividend",
            &[PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)],
        )
        .await;
        assert_eq!(deny_div_pay.unwrap_err().code, "CorpRoleDenied");

        let insuf_div_pay = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "PayoutDividend",
            &[PyRep::Bool(true), PyRep::Real(50_000_000.0), PyRep::Integer(1)],
        )
        .await;
        assert_eq!(insuf_div_pay.unwrap_err().code, "InsufficientFunds");

        let p1_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());
        rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)])
            .await
            .expect("PayoutDividend");
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 250_000_00, "PILOT_2 (250/1000 shares) must receive 250,000 ISK");
            assert_eq!(s.characters[&PILOT_1].balance.raw_cents() - p1_bal_before, 150_000_00, "PILOT_1 (150/1000 shares) must receive 150,000 ISK");
            assert_eq!(s.corporations[&corp_id].division_balance(1).raw_cents(), 9_600_000_00, "Corp wallet must be debited by 400,000 ISK");
            assert!(s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "CorporationDividendPayment" && j.amount.raw_cents() == 250_000_00));
            assert!(s.wallet_journal.iter().any(|j| j.character_id == PILOT_1 && j.entry_type == "CorporationDividendPayment" && j.amount.raw_cents() == 150_000_00));
            assert!(s.wallet_journal.iter().any(|j| j.character_id == corp_id && j.entry_type == "CorporationDividendPayout" && j.amount.raw_cents() == -400_000_00));
        });

        // 4. CEO candidacy (CanRunForCEO), resignation gating, and CEO election via vote case
        ctx.db.with_mut_snapshot(|s| {
            if let Some(sk) = s.skills.get_mut(&PILOT_2) {
                sk.remove(&3363);
            }
        });
        assert_eq!(
            rpc(&ctx, &mut sess2, "voteManager", "CanRunForCEO", &[]).await.unwrap(),
            PyRep::Bool(false),
            "CanRunForCEO must be false without Corporation Management skill (3363)"
        );
        ctx.db.with_mut_snapshot(|s| {
            s.skills.entry(PILOT_2).or_default().insert(
                3363,
                eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 },
            );
        });
        assert_eq!(
            rpc(&ctx, &mut sess2, "voteManager", "CanRunForCEO", &[]).await.unwrap(),
            PyRep::Bool(true),
            "CanRunForCEO must be true with Corporation Management skill (3363)"
        );

        let pot_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
            .await
            .expect("GetNumberOfPotentialCEOs");
        assert_eq!(pot_ceos, PyRep::Integer(1));
        let deny_resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[]).await;
        assert_eq!(deny_resign.unwrap_err().code, "InvalidNewCEO");

        // Elect PILOT_2 as CEO via CEO vote case (vote_type = 0)
        let vc_rep = rpc(
            &ctx,
            &mut sess1,
            "voteManager",
            "InsertVoteCase",
            &[
                PyRep::Integer(corp_id),
                PyRep::String("Elect PILOT_2 as CEO".into()),
                PyRep::String("CEO succession vote".into()),
                PyRep::Integer(0),
                PyRep::Integer(PILOT_2),
            ],
        )
        .await
        .expect("InsertVoteCase (CEO election)");
        let case_id = field(&vc_rep, "voteCaseID").as_i64().unwrap();
        rpc(
            &ctx,
            &mut sess2,
            "voteManager",
            "InsertVote",
            &[PyRep::Integer(corp_id), PyRep::Integer(case_id), PyRep::Integer(1)],
        )
        .await
        .expect("InsertVote for PILOT_2");
        ctx.advance_secs(86_401);
        rpc(
            &ctx,
            &mut sess2,
            "voteManager",
            "ActivateSanctionedAction",
            &[PyRep::Integer(case_id)],
        )
        .await
        .expect("ActivateSanctionedAction (CEO election)");
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.corporations[&corp_id].ceo_id, PILOT_2, "Vote case must elect PILOT_2 as CEO");
        });

        // PILOT_2 resigns and designates PILOT_1 as successor
        let handback = rpc(&ctx, &mut sess2, "corpRegistry", "ResignFromCEO", &[PyRep::Integer(PILOT_1)])
            .await
            .expect("ResignFromCEO(PILOT_1)");
        assert_eq!(field(&handback, "newCeoID"), PyRep::Integer(PILOT_1));
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.corporations[&corp_id].ceo_id, PILOT_1);
        });

        // 5. Pending auto-kick when member is in space
        ctx.advance_secs(12);
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&PILOT_2).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
        });
        rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)])
            .await
            .expect("KickOutMember queued");
        let pending = rpc(&ctx, &mut sess1, "corpRegistry", "GetPendingAutoKicks", &[])
            .await
            .expect("GetPendingAutoKicks");
        assert!(rowset_rows(&pending).iter().any(|r| r.first() == Some(&PyRep::Integer(PILOT_2))));

        let executed = eve_server::services::fleet_social::execute_pending_auto_kicks_on_downtime(&ctx);
        assert!(executed >= 1);
        ctx.db.with_snapshot(|s| {
            assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
        });

        // 6. Solo CEO resignation -> vacant CEO state (newCeoID == 0)
        let pot_ceos_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
            .await
            .expect("GetNumberOfPotentialCEOs after kick");
        assert_eq!(pot_ceos_after, PyRep::Integer(0));
        let resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[])
            .await
            .expect("ResignFromCEO");
        assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(0));
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.corporations[&corp_id].ceo_id, 0);
        });
    }

    #[tokio::test]
    async fn test_b22_corp_projects_and_npc_corp_rules() {
        // parity: feat:corporation.corp-project-close-expiry, feat:corporation.corp-project-reward-pool
        eve_testkit::attest(&[
            "feat:corporation.corp-project-close-expiry",
            "feat:corporation.corp-project-reward-pool",
        ]);

        let ctx = ServerContext::new();
        let mut sess1 = select_pilot(&ctx, 72_301, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = select_pilot(&ctx, 72_302, "other", "other-secret", PILOT_2).await;

        let created = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Helios Extraction".into()),
                PyRep::String("HLOS".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation");
        let corp_id = field(&created, "corporationID").as_i64().unwrap();

        let dock_id = sess2.station_id.unwrap_or(60_003_760);
        let mut trit_item_id = 0_i64;
        ctx.db.with_mut_snapshot(|s| {
            let corp = s.corporations.get_mut(&corp_id).unwrap();
            corp.members.insert(PILOT_2);
            corp.ensure_wallet_divisions();
            corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
            corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
            s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
            s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
            s.next_entity_id += 1;
            trit_item_id = s.next_entity_id;
            s.items.insert(
                trit_item_id,
                eve_db::InventoryItemRecord {
                    item_id: trit_item_id,
                    type_id: 34,
                    owner_id: PILOT_2,
                    location_id: dock_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        });
        sess2.corporation_id = Some(corp_id);

        // 1. Create projects for MineMaterials, DestroyNPCShips, EarnLoyaltyPoints, and DeliverItem
        let create_mine = rpc(
            &ctx,
            &mut sess1,
            "corporationSvc",
            "CreateGoalSubmit",
            &[
                PyRep::String("Mine Veldspar".into()),
                PyRep::Integer(100),
                PyRep::Real(10_000.0),
                PyRep::String("MineMaterials".into()),
                PyRep::Bool(false),
                PyRep::Integer(1),
                PyRep::Integer(50),
                PyRep::Integer(86_400),
            ],
        )
        .await
        .expect("CreateGoalSubmit MineMaterials");
        let pid_mine = field(&create_mine, "projectID").as_i64().unwrap();

        let create_npc = rpc(
            &ctx,
            &mut sess1,
            "corporationSvc",
            "CreateGoalSubmit",
            &[
                PyRep::String("Clear Rats".into()),
                PyRep::Integer(10),
                PyRep::Real(5_000.0),
                PyRep::String("DestroyNPCShips".into()),
                PyRep::Bool(false),
                PyRep::Integer(1),
                PyRep::Integer(10),
                PyRep::Integer(86_400),
            ],
        )
        .await
        .expect("CreateGoalSubmit DestroyNPCShips");
        let pid_npc = field(&create_npc, "projectID").as_i64().unwrap();

        let create_lp = rpc(
            &ctx,
            &mut sess1,
            "corporationSvc",
            "CreateGoalSubmit",
            &[
                PyRep::String("Earn LP".into()),
                PyRep::Integer(100),
                PyRep::Real(1_000.0),
                PyRep::String("EarnLoyaltyPoints".into()),
                PyRep::Bool(false),
                PyRep::Integer(1),
                PyRep::Integer(100),
                PyRep::Integer(86_400),
            ],
        )
        .await
        .expect("CreateGoalSubmit EarnLoyaltyPoints");
        let pid_lp = field(&create_lp, "projectID").as_i64().unwrap();

        let create_deliv = rpc(
            &ctx,
            &mut sess1,
            "corporationSvc",
            "CreateGoalSubmit",
            &[
                PyRep::String("Deliver Tritanium".into()),
                PyRep::Integer(50),
                PyRep::Real(1_000.0),
                PyRep::String("DeliverItem".into()),
                PyRep::Bool(false),
                PyRep::Integer(1),
                PyRep::Integer(50),
                PyRep::Integer(86_400),
            ],
        )
        .await
        .expect("CreateGoalSubmit DeliverItem");
        let pid_deliv = field(&create_deliv, "projectID").as_i64().unwrap();

        // Client-reported GoalProgress on server-tracked MineMaterials must be rejected
        let client_spoof = rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_mine), PyRep::Integer(10)]).await;
        assert_eq!(client_spoof.unwrap_err().code, "ServerTrackedMatcher");

        // Drive real server paths for MineMaterials, DestroyNPCShips, EarnLoyaltyPoints, and DeliverItem
        eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 10, 30000142, 1230);
        eve_server::services::combat_npc_encounters::record_npc_kill_progression(&ctx, PILOT_2, Some(10001));
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|s| {
            eve_server::services::lp_store::record_lp_reward_log(s, PILOT_2, 1_000_006, 25, "LP reward".into(), now_unix);
        });
        let over_deliv = rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_deliv), PyRep::Integer(20)]).await;
        assert_eq!(over_deliv.unwrap_err().code, "InsufficientContributionItems");
        rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_deliv), PyRep::Integer(5)])
            .await
            .expect("GoalProgress DeliverItem");

        ctx.db.with_snapshot(|s| {
            assert_eq!(s.corp_projects[&pid_mine].current_quantity, 10);
            assert_eq!(s.corp_projects[&pid_npc].current_quantity, 1);
            assert_eq!(s.corp_projects[&pid_lp].current_quantity, 25);
            assert_eq!(s.corp_projects[&pid_deliv].current_quantity, 5);
            assert_eq!(s.items[&trit_item_id].quantity, 5, "DeliverItem must consume 5 Tritanium from PILOT_2 hangar");
            assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 135_000_00);
        });

        // Close project and verify remaining escrow (900,000 ISK) refunded
        let bal_before_close = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents());
        rpc(&ctx, &mut sess1, "corporationSvc", "CloseCorpProject", &[PyRep::Integer(pid_mine)])
            .await
            .expect("CloseCorpProject");
        ctx.db.with_snapshot(|s| {
            let bal_after_close = s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents();
            assert_eq!(bal_after_close - bal_before_close, 900_000_00);
        });

        // 2. NPC corp placement rules & 11% NPC corp tax rate
        let npc_app = rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "InsertApplication",
            &[PyRep::Integer(1_000_006), PyRep::String("Join NPC".into())],
        )
        .await;
        assert!(npc_app.is_err(), "Cannot apply to NPC corporation");

        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&PILOT_2).unwrap().corporation_id = 1_000_006;
        });
        let sys_id = 30000142;
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 50_000.0);
        });
        let (small_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
        assert!((small_net - 50_000.0).abs() < 1e-3, "Bounty under 100k ISK must be tax-exempt");

        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
        });
        let (large_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
        assert!((large_net - 890_000.0).abs() < 1e-3, "11% NPC corp tax must apply above 100k ISK threshold");
        ctx.db.with_snapshot(|s| {
            assert!(
                s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "bounty_prize_corporation_tax" && j.amount.raw_cents() == -110_000_00),
                "Wallet journal must record 11% NPC corp tax deduction"
            );
        });
    }
}

pub(crate) mod mining_goals_market_excavator_npc {
    // End-to-end integration tests for Batch 22 open `feat:mining.*` and `feat:npc.mining-op-npc` rows.


    use crate::corp_mining_lifecycle_rpcs::{rpc, select_pilot, PILOT_1, PILOT_2};
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::session_cluster_lifecycle::tick_background_cluster;
    use glam::DVec3;

    const JITA_SYS: u32 = 30_000_142;
    const JITA_STATION: i64 = 60_003_760;

    #[tokio::test]
    async fn test_b22_mining_goals_and_market_e2e() {
        // parity: feat:mining.mining-market
        eve_testkit::attest(&["feat:mining.mining-market"]);

        let ctx = ServerContext::new();
        let mut s1 = select_pilot(&ctx, 55_101, "demo", "demo-secret", PILOT_1).await;
        let mut s2 = select_pilot(&ctx, 55_102, "other", "other-secret", PILOT_2).await;

        // 1. Negative case for feat:mining.mining-goals: client-supplied count via seasonManager.process_client_event
        // must NOT advance mining goals when mining_ledger is empty.
        let _ = rpc(
            &ctx,
            &mut s1,
            "seasonManager",
            "process_client_event",
            &[PyRep::String("totalMined".into()), PyRep::Integer(50_000)],
        )
        .await;
        let ledger_before = ctx.db.get_mining_ledger_with_waste(PILOT_1);
        assert!(ledger_before.is_empty(), "Client-supplied event must not populate mining_ledger");

        // 2. Seed a Venture (32880) with Miner I (483) in space in Jita and mine an asteroid via live RPC
        let ship_id = 77_001_001_i64;
        let miner_id = 77_001_002_i64;
        let ast_id = 77_001_003_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.station_id = None;
                c.structure_id = None;
                c.solar_system_id = JITA_SYS;
                c.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 32880,
                    owner_id: PILOT_1,
                    location_id: JITA_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                miner_id,
                InventoryItemRecord {
                    item_id: miner_id,
                    type_id: 483,
                    owner_id: PILOT_1,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(miner_id, true);
            snap.skills.entry(PILOT_1).or_default().insert(
                3386,
                eve_db::TrainedSkillRecord {
                    type_id: 3386, // Mining
                    level: 4,
                    skill_points: 45_255.0,
                },
            );
        });
        s1.solar_system_id = Some(JITA_SYS);
        s1.ship_id = Some(ship_id);
        ctx.invalidate_ship_fitting(ship_id);

        let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let (ship_ball, ship_combat) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship_id, 32880, DVec3::ZERO);
        jita_handle.spawn_ball_with_combat(ship_ball, ship_combat).await.expect("spawn Venture");
        let (ast_ball, _) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
        jita_handle
            .spawn_asteroid(
                eve_server::system_actor::AsteroidState {
                    asteroid_id: ast_id,
                    belt_id: 0,
                    type_id: 1230,
                    initial_units: 20_000,
                    remaining_units: 20_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                ast_ball,
            )
            .await
            .expect("spawn Veldspar asteroid");

        let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
            .await
            .expect("ShipOnlineModules");
        let _ = rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ast_id)])
            .await
            .expect("AddTarget asteroid");
        ctx.advance_secs(5);
        jita_handle.step_manual(5.0).await.expect("step target lock");
        let mine_res = rpc(
            &ctx,
            &mut s1,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(miner_id), PyRep::Integer(ast_id)],
        )
        .await
        .expect("dogmaIM.Activate Miner I");
        assert!(!matches!(mine_res, PyRep::None), "Mining activation must return payload");
        ctx.advance_secs(60);
        tick_background_cluster(&ctx).await;

        let ledger_after = ctx.db.get_mining_ledger_with_waste(PILOT_1);
        assert!(!ledger_after.is_empty(), "Mining cycle must record entry in mining_ledger");
        let mined_qty: i64 = ledger_after.iter().map(|r| r.0.quantity).sum();
        assert!(mined_qty > 0, "Mined quantity in ledger must be positive");

        let challenges = rpc(
            &ctx,
            &mut s1,
            "seasonManager",
            "fill_up_and_return_challenges_for_character",
            &[],
        )
        .await
        .expect("fill_up_and_return_challenges_for_character");
        assert!(!matches!(challenges, PyRep::None), "Seasonal/daily challenges must be returned");

        // 3. Test feat:mining.mining-market: reprocess Veldspar into Tritanium and trade ore, compressed ore, and minerals on market
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.station_id = Some(JITA_STATION);
                c.solar_system_id = JITA_SYS;
            }
            if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
                c2.station_id = Some(JITA_STATION);
                c2.solar_system_id = JITA_SYS;
                c2.balance = IskCents::from_isk(10_000_000);
            }
            snap.items.insert(
                77_001_010,
                InventoryItemRecord {
                    item_id: 77_001_010,
                    type_id: 1230, // Veldspar (0.1 m3)
                    owner_id: PILOT_1,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_000,
                    singleton: false,
                    ..Default::default()
                },
            );
            snap.items.insert(
                77_001_011,
                InventoryItemRecord {
                    item_id: 77_001_011,
                    type_id: 62516, // Compressed Veldspar (0.001 m3)
                    owner_id: PILOT_1,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        s1.station_id = Some(JITA_STATION);
        s2.station_id = Some(JITA_STATION);

        // Reprocess 500 Veldspar into Tritanium (34) via reprocessingSvc.Reprocess
        let _ = rpc(
            &ctx,
            &mut s1,
            "reprocessingSvc",
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(77_001_010)]),
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(InventoryFlag::Hangar as i64),
                PyRep::Bool(false),
                PyRep::List(vec![]),
            ],
        )
        .await
        .expect("reprocessingSvc.Reprocess");

        let trit_item_id = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 34 && i.quantity > 0)
                .map(|i| i.item_id)
                .expect("Tritanium produced by reprocessing Veldspar")
        });

        // Place sell orders for Compressed Veldspar (62516) and Tritanium (34)
        let _ = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(62516)),
                    ("itemID", PyRep::Integer(77_001_011)),
                    ("price", PyRep::Real(15.0)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
            ],
        )
        .await
        .expect("PlaceMultiSellOrder Compressed Veldspar");

        let _ = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(34)),
                    ("itemID", PyRep::Integer(trit_item_id)),
                    ("price", PyRep::Real(5.0)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
            ],
        )
        .await
        .expect("PlaceMultiSellOrder Tritanium");

        let orders_comp = rpc(
            &ctx,
            &mut s2,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(62516)],
        )
        .await
        .expect("GetOrders Compressed Veldspar");
        assert!(!matches!(orders_comp, PyRep::None), "Compressed Veldspar must be listed on market");

        let orders_trit = rpc(
            &ctx,
            &mut s2,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(34)],
        )
        .await
        .expect("GetOrders Tritanium");
        assert!(!matches!(orders_trit, PyRep::None), "Tritanium must be listed on market");
    }

    #[tokio::test]
    async fn test_b22_mining_excavator_npc_hauler_and_belt_ores() {
        let ctx = ServerContext::new();
        let mut s1 = select_pilot(&ctx, 55_103, "demo", "demo-secret", PILOT_1).await;

        // 1. Test DERIVED belt ore distribution across security bands & empire regions
        let ores_1_0 = ctx.sde.belt_ores_for_security_and_region(1.0, 10_000_002);
        let ids_1_0: Vec<u32> = ores_1_0.iter().map(|t| t.type_id).collect();
        assert_eq!(ids_1_0, vec![1230, 1228], "1.0 high-sec must only contain Veldspar (1230) and Scordite (1228)");

        let ores_amarr_07 = ctx.sde.belt_ores_for_security_and_region(0.7, 10_000_043); // Domain (Amarr)
        let ids_amarr_07: Vec<u32> = ores_amarr_07.iter().map(|t| t.type_id).collect();
        assert!(ids_amarr_07.contains(&1224) && !ids_amarr_07.contains(&20), "0.7 Amarr must contain Pyroxeres and no Kernite");

        let ores_low = ctx.sde.belt_ores_for_security_and_region(0.3, 10_000_032); // Sinq Laison (Low-sec)
        let ids_low: Vec<u32> = ores_low.iter().map(|t| t.type_id).collect();
        assert!(ids_low.contains(&20) && ids_low.contains(&1227) && !ids_low.contains(&1230), "0.3 low-sec must contain Kernite/Omber and no Veldspar");

        let ores_null = ctx.sde.belt_ores_for_security_and_region(-0.8, 10_000_050); // Querious (Null)
        let ids_null: Vec<u32> = ores_null.iter().map(|t| t.type_id).collect();
        assert!(ids_null.contains(&22) && ids_null.contains(&11396), "Deep null-sec must contain Arkonor and Mercoxit");

        // 2. Test feat:mining.excavator-drones: negative case (ship_id 100_000_001 without mining module -> NoMiningModuleFitted)
        let unequipped_ship = 100_000_001_i64;
        let ast_id = 77_002_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.station_id = None;
                c.structure_id = None;
                c.solar_system_id = JITA_SYS;
                c.active_ship_id = unequipped_ship;
            }
            snap.items.insert(
                unequipped_ship,
                InventoryItemRecord {
                    item_id: unequipped_ship,
                    type_id: 32880,
                    owner_id: PILOT_1,
                    location_id: JITA_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        s1.solar_system_id = Some(JITA_SYS);
        s1.ship_id = Some(unequipped_ship);

        let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let (sb, sc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, unequipped_ship, 32880, DVec3::ZERO);
        jita_handle.spawn_ball_with_combat(sb, sc).await.expect("spawn unequipped ship");
        let (ab, _) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
        jita_handle
            .spawn_asteroid(
                eve_server::system_actor::AsteroidState {
                    asteroid_id: ast_id,
                    belt_id: 0,
                    type_id: 1230,
                    initial_units: 50_000,
                    remaining_units: 50_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                ab,
            )
            .await
            .expect("spawn asteroid");

        let err = rpc(
            &ctx,
            &mut s1,
            "entity",
            "CmdMineRepeatedly",
            &[PyRep::List(vec![]), PyRep::Integer(ast_id)],
        )
        .await
        .expect_err("Unequipped ship 100_000_001 must fail with NoMiningModuleFitted");
        assert_eq!(err.code, "NoMiningModuleFitted");

        // 3. Test feat:npc.mining-op-npc: companion hauler warps to destination station when ore hold fills and offloads
        let tanoo_sys = 30_000_001_u32;
        let tanoo_handle = ctx.systems.get_or_spawn(tanoo_sys);
        let _ = tanoo_handle.populate_asteroid_belt_entities().await.expect("populate belts");
        let convoy_id = ctx.living_world.ensure_system_mining_op(tanoo_sys, &ctx.sde);
        ctx.living_world.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("step 0");

        assert_eq!(ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id), 0, "Hauler starts empty at belt");
        // Advance ticks until Venture ore hold fills and hauler loads ore for multi-tick warp transit
        let mut loaded_in_transit = false;
        for _ in 0..90 {
            ctx.advance_secs(60);
            let step = ctx
                .living_world
                .step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
                .await
                .expect("step mining");
            if ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id) > 0 {
                loaded_in_transit = true;
                assert!(!step.hauled_to_station, "Hauler must not teleport ore in 0 ticks on departure");
                assert_eq!(step.convoy_cargo_units, 0);
                break;
            }
        }
        assert!(loaded_in_transit, "Hauler must load mined ore into eve-db when miner hold fills");
        let in_transit_qty = ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id);
        assert!(in_transit_qty > 0);

        // Next tick completes multi-tick warp transit and delivers ore to destination station
        ctx.advance_secs(60);
        let deliver_step = ctx
            .living_world
            .step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
            .await
            .expect("step deliver");
        assert!(deliver_step.hauled_to_station, "Hauler must deliver ore after multi-tick transit");
        assert_eq!(ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id), 0, "Hauler cargo is empty after offload");

        // 4. Test destroying an in-transit hauler drops a wreck with its persisted ore cargo
        let convoy_id_2 = ctx.living_world.ensure_system_mining_op(30_000_144, &ctx.sde);
        let perim_handle = ctx.systems.get_or_spawn(30_000_144);
        let _ = perim_handle.populate_asteroid_belt_entities().await.expect("populate perim belts");
        ctx.living_world.step_npc_mining_op(convoy_id_2, &ctx.systems, &ctx.db, &ctx.sde).await.expect("p step 0");
        for _ in 0..90 {
            ctx.advance_secs(60);
            ctx.living_world.step_npc_mining_op(convoy_id_2, &ctx.systems, &ctx.db, &ctx.sde).await.expect("p step");
            if ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id_2) > 0 {
                break;
            }
        }
        let dropped_expected = ctx.living_world.hauler_cargo_units(&ctx.db, convoy_id_2);
        assert!(dropped_expected > 0, "Second hauler must be loaded in transit");
        let hauler_ball_2 = convoy_id_2 + 1_000_000;
        let (wreck_id, _, _) = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_with_hint(&ctx, 30_000_144, hauler_ball_2, 90_000_001, None).await;
        assert!(wreck_id > 0, "Destroying in-transit hauler must spawn a wreck");
        let wreck_ore_qty: i32 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == wreck_id && i.type_id != 34 && i.type_id != 35)
                .map(|i| i.quantity)
                .sum()
        });
        assert!(wreck_ore_qty >= dropped_expected, "In-transit hauler wreck must contain its mined ore cargo");
    }
}

pub(crate) mod mobile_observatory_standup_point_defense {
    // End-to-end integration tests for Batch 22 open `feat:structures.*` and `feat:modules.*` rows.


    use crate::corp_mining_lifecycle_rpcs::{rpc, select_pilot, PILOT_1, PILOT_2};
    use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::session_cluster_lifecycle::tick_background_cluster;
    use glam::DVec3;

    const JITA_SYS: u32 = 30_000_142;
    const NULL_SYS: u32 = 30_004_759;

    #[tokio::test]
    async fn test_b22_mobile_observatory_and_standup_point_defense() {
        // parity: feat:structures.mobile-observatory, feat:structures.point-defense-system
        eve_testkit::attest(&[
            "feat:structures.mobile-observatory",
            "feat:structures.point-defense-system",
        ]);

        let ctx = ServerContext::new();
        let mut s1 = select_pilot(&ctx, 55_201, "demo", "demo-secret", PILOT_1).await;
        let mut s2 = select_pilot(&ctx, 55_202, "other", "other-secret", PILOT_2).await;

        // 1. Negative case for feat:structures.mobile-observatory: deploying in high-sec (Jita) is rejected
        let ship1_id = 77_101_001_i64;
        let obs_item_id = 77_101_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.station_id = None;
                c.structure_id = None;
                c.solar_system_id = JITA_SYS;
                c.active_ship_id = ship1_id;
            }
            snap.items.insert(
                ship1_id,
                InventoryItemRecord {
                    item_id: ship1_id,
                    type_id: 587, // Rifter
                    owner_id: PILOT_1,
                    location_id: JITA_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                obs_item_id,
                InventoryItemRecord {
                    item_id: obs_item_id,
                    type_id: eve_server::services::mobile_deployables::TYPE_MOBILE_OBSERVATORY,
                    owner_id: PILOT_1,
                    location_id: ship1_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.skills
                .entry(PILOT_1)
                .or_default()
                .insert(
                    11584,
                    TrainedSkillRecord {
                        type_id: 11584, // Anchoring
                        level: 3,
                        skill_points: 24_000.0,
                    },
                );
        });
        s1.solar_system_id = Some(JITA_SYS);
        s1.ship_id = Some(ship1_id);

        let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let (sb1, sc1) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship1_id, 587, DVec3::ZERO);
        jita_handle.spawn_ball_with_combat(sb1, sc1).await.expect("spawn ship1");

        let hs_err = rpc(
            &ctx,
            &mut s1,
            "posMgr",
            "AnchorStructure",
            &[
                PyRep::Integer(obs_item_id),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .expect_err("Mobile Observatory in high-sec must be rejected");
        assert_eq!(hs_err.code, "CannotDeployInHighSec");

        // 2. Positive case for feat:structures.mobile-observatory: deploy in null-sec, anchor, and decloak nearby ship
        let ship2_id = 77_101_010_i64;
        let cloak_id = 77_101_011_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.solar_system_id = NULL_SYS;
            }
            if let Some(it) = snap.items.get_mut(&ship1_id) {
                it.location_id = NULL_SYS as i64;
            }
            if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
                c2.station_id = None;
                c2.structure_id = None;
                c2.solar_system_id = NULL_SYS;
                c2.active_ship_id = ship2_id;
            }
            snap.items.insert(
                ship2_id,
                InventoryItemRecord {
                    item_id: ship2_id,
                    type_id: 11957, // Falcon (Force Recon, can fit Covert Ops Cloaking Device II)
                    owner_id: PILOT_2,
                    location_id: NULL_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                cloak_id,
                InventoryItemRecord {
                    item_id: cloak_id,
                    type_id: 11578, // Covert Ops Cloaking Device II
                    owner_id: PILOT_2,
                    location_id: ship2_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        s1.station_id = None;
        s1.solar_system_id = Some(NULL_SYS);
        s2.station_id = None;
        s2.solar_system_id = Some(NULL_SYS);
        s2.ship_id = Some(ship2_id);

        let null_handle = ctx.systems.get_or_spawn(NULL_SYS);
        let (sb1n, sc1n) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship1_id, 587, DVec3::ZERO);
        null_handle.spawn_ball_with_combat(sb1n, sc1n).await.expect("spawn ship1 in null");
        let (sb2n, mut sc2n) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship2_id, 11957, DVec3::new(5_000_000_000.0, 0.0, 0.0));
        sc2n.capacitor = 500.0;
        sc2n.max_capacitor = 500.0;
        null_handle.spawn_ball_with_combat(sb2n, sc2n).await.expect("spawn ship2 in null");
        ctx.invalidate_ship_fitting(ship2_id);

        // Cloak ship2 via live dogmaIM.ShipOnlineModules + dogmaIM.Activate RPC
        let _ = rpc(&ctx, &mut s2, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship2_id)])
            .await
            .expect("ShipOnlineModules ship2");
        let _ = rpc(
            &ctx,
            &mut s2,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(cloak_id), PyRep::String("default".into()), PyRep::None, PyRep::Integer(1_000_000)],
        )
        .await
        .expect("Activate Covert Ops Cloak");
        assert!(
            null_handle.get_combat_state(ship2_id).await.unwrap().expect("ship2 combat").is_cloaked,
            "ship2 must be cloaked after dogmaIM.Activate"
        );

        let _ = rpc(
            &ctx,
            &mut s1,
            "posMgr",
            "AnchorStructure",
            &[
                PyRep::Integer(obs_item_id),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .expect("Anchor Mobile Observatory in null-sec");

        // Advance 605s (past 10m anchor delay, but < 15m / 900s cloak stabilization) -> ship2 must remain cloaked
        ctx.advance_secs(605);
        tick_background_cluster(&ctx).await;
        let cs2_mid = null_handle.get_combat_state(ship2_id).await.unwrap().expect("ship2 combat mid");
        assert!(cs2_mid.is_cloaked, "Ship cloaked for < 15 minutes must not be decloaked by Mobile Observatory");
        assert!(cs2_mid.cloak_stabilization_remaining_s > 0.0, "Cloak stabilization timer must still be positive at 605s");

        // Advance past 15-minute (900s) cloak stabilization -> observatory ping decloaks ship2
        ctx.mechanic_rng.set_seed(Some(42));
        ctx.advance_secs(3_000);
        for _ in 0..50 {
            null_handle.step_manual(60.0).await.unwrap();
        }
        tick_background_cluster(&ctx).await;

        let cs2_after = null_handle.get_combat_state(ship2_id).await.unwrap().expect("ship2 combat state");
        assert!(!cs2_after.is_cloaked, "Mobile Observatory ping must decloak ship within range after 15-min stabilization expires");

        // 3. Test feat:structures.point-defense-system & feat:structures.structure-weapons
        let keepstar_id = 77_102_001_i64;
        let pds_id = 77_102_002_i64;
        let flak_id = 77_102_003_i64;
        let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.corporation_id).unwrap_or(98_000_001));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.member_roles.insert(PILOT_1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
            }
            snap.structures.insert(
                keepstar_id,
                UpwellStructureRecord {
                    structure_id: keepstar_id,
                    type_id: 35834, // Keepstar (XL)
                    owner_corp_id: corp_id,
                    solar_system_id: NULL_SYS,
                    name: "Test Keepstar".into(),
                    position: [100_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.items.insert(
                keepstar_id,
                InventoryItemRecord {
                    item_id: keepstar_id,
                    type_id: 35834, // Keepstar (XL)
                    owner_id: corp_id,
                    location_id: NULL_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                pds_id,
                InventoryItemRecord {
                    item_id: pds_id,
                    type_id: 35926, // Standup Point Defense Battery I
                    owner_id: corp_id,
                    location_id: keepstar_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(pds_id, true);
            snap.items.insert(
                flak_id,
                InventoryItemRecord {
                    item_id: flak_id,
                    type_id: 63195, // Standup Flak Rounds
                    owner_id: corp_id,
                    location_id: keepstar_id,
                    flag: InventoryFlag::SpecializedAmmoHold,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.structure_id = Some(keepstar_id);
                c.active_ship_id = keepstar_id;
            }
        });
        s1.structure_id = Some(keepstar_id);
        s1.ship_id = Some(keepstar_id);
        s1.corp_role |= corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

        let (kb, mut kc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, keepstar_id, 35834, DVec3::new(100_000.0, 0.0, 0.0));
        kc.capacitor = 10_000.0;
        null_handle.spawn_ball_with_combat(kb, kc).await.expect("spawn Keepstar");
        // Move ship2 within 1,100m of Keepstar so PDS damages it
        assert!(null_handle.set_ball_position(ship2_id, DVec3::new(101_100.0, 0.0, 0.0)).await, "move ship2 near Keepstar");
        let before_sh = null_handle.get_combat_state(ship2_id).await.unwrap().unwrap().shield_hp;

        let _ = rpc(
            &ctx,
            &mut s1,
            "structureControl",
            "TakeControl",
            &[PyRep::Integer(keepstar_id)],
        )
        .await
        .expect("TakeControl Keepstar");

        let pds_res = rpc(
            &ctx,
            &mut s1,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(pds_id)],
        )
        .await
        .expect("Activate Standup Point Defense Battery");
        assert!(!matches!(pds_res, PyRep::None));

        let after_sh = null_handle.get_combat_state(ship2_id).await.unwrap().unwrap().shield_hp;
        assert!(after_sh < before_sh, "Standup Point Defense Battery must damage ship within perimeter");
    }

    async fn run_overheat_fit(thermo_lvl: u8) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
        let ctx = ServerContext::new();
        let mut s1 = select_pilot(&ctx, 55_203, "demo", "demo-secret", PILOT_1).await;

        let ship_id = 77_103_001_i64;
        let mod_high = 77_103_005_i64;
        let mod_left = 77_103_002_i64;
        let mod_center = 77_103_003_i64;
        let mod_right = 77_103_004_i64;
        let mod_low = 77_103_006_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_1) {
                c.station_id = None;
                c.structure_id = None;
                c.solar_system_id = JITA_SYS;
                c.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587, // Rifter (1259 heatAttenuationHi=0.5, 1261 heatAttenuationMed=0.5)
                    owner_id: PILOT_1,
                    location_id: JITA_SYS as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            for (mid, flag, tid) in [
                (mod_high, InventoryFlag::HiSlot0, 2873),   // 125mm Gatling AutoCannon II (flag 27, 1180=0.02, 0 cap)
                (mod_left, InventoryFlag::MedSlot0, 3242),  // Warp Disruptor I (flag 19, dist 1)
                (mod_center, InventoryFlag::MedSlot1, 440), // 5MN Microwarpdrive II (flag 20, 1180=0.04)
                (mod_right, InventoryFlag::MedSlot2, 527),  // Stasis Webifier II (flag 21, dist 1)
                (mod_low, InventoryFlag::LoSlot0, 2048),    // Damage Control II (flag 11, low rack)
            ] {
                let mut item = InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: PILOT_1,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                };
                item.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 500.0);
                snap.items.insert(mid, item);
                snap.module_online_states.insert(mid, true);
            }
            snap.items.insert(
                77_103_010,
                InventoryItemRecord {
                    item_id: 77_103_010,
                    type_id: 212, // EMP S
                    owner_id: PILOT_1,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 100,
                    singleton: false,
                    ..Default::default()
                },
            );
            snap.skills.entry(PILOT_1).or_default().insert(
                28164,
                TrainedSkillRecord {
                    type_id: 28164, // Thermodynamics
                    level: thermo_lvl,
                    skill_points: 135_765.0,
                },
            );
            snap.skills.entry(PILOT_1).or_default().insert(
                3454,
                TrainedSkillRecord {
                    type_id: 3454, // High Speed Maneuvering
                    level: 4,
                    skill_points: 135_765.0,
                },
            );
        });
        s1.solar_system_id = Some(JITA_SYS);
        s1.ship_id = Some(ship_id);
        ctx.invalidate_ship_fitting(ship_id);

        let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let (sb, mut sc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship_id, 587, DVec3::ZERO);
        sc.capacitor = 2_000.0;
        sc.max_capacitor = 2_000.0;
        jita_handle.spawn_ball_with_combat(sb, sc).await.expect("spawn Rifter");
        let target_id = 77_103_099_i64;
        let (tb, mut tc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, target_id, 587, DVec3::new(1_000.0, 0.0, 0.0));
        tc.shield_hp = 50_000.0;
        tc.max_shield_hp = 50_000.0;
        jita_handle.spawn_ball_with_combat(tb, tc).await.expect("spawn target");
        jita_handle.add_target_lock(ship_id, eve_destiny::TargetLock { target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await.expect("lock target");

        let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
            .await
            .expect("ShipOnlineModules");
        let _ = rpc(&ctx, &mut s1, "dogmaIM", "Overload", &[PyRep::Integer(mod_center), PyRep::None])
            .await
            .expect("dogmaIM.Overload mid");
        let _ = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(mod_high), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1)])
            .await
            .expect("dogmaIM.Activate high");
        let _ = rpc(&ctx, &mut s1, "dogmaIM", "Overload", &[PyRep::Integer(mod_high), PyRep::None])
            .await
            .expect("dogmaIM.Overload high");

        // Step SystemActor across 80s (80 x 1.0s ticks) so high & mid (10s cycle -> 8 cycles = 80s) rack heat build
        for _ in 0..80 {
            ctx.advance_secs(1);
            jita_handle.step_manual(1.0).await.expect("step overheat cycle");
        }

        ctx.db.with_snapshot(|snap| {
            let tr = snap.transient_item_attributes.get(&ship_id);
            let hi_h = tr.and_then(|m| m.get(&1175)).copied().unwrap_or(0.0);
            let med_h = tr.and_then(|m| m.get(&1176)).copied().unwrap_or(0.0);
            let low_h = tr.and_then(|m| m.get(&1177)).copied().unwrap_or(0.0);
            let dl = snap.items.get(&mod_left).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
            let dc = snap.items.get(&mod_center).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
            let dr = snap.items.get(&mod_right).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
            let dlow = snap.items.get(&mod_low).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
            let dhi = snap.items.get(&mod_high).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
            (hi_h, med_h, low_h, dl, dc, dr, dlow, dhi)
        })
    }

    #[tokio::test]
    async fn test_b22_overheat_rack_heat_and_damage_propagation() {
        // parity: feat:modules.overheat-rack-heat-generation, feat:modules.overheat-damage-propagation
        eve_testkit::attest(&[
            "feat:modules.overheat-rack-heat-generation",
            "feat:modules.overheat-damage-propagation",
        ]);

        let (hi_heat, med_heat, low_heat, dmg_left_0, dmg_center_0, dmg_right_0, dmg_low_0, _dmg_hi_0) =
            run_overheat_fit(0).await;
        let (_hi_heat_4, _med_heat_4, _low_heat_4, _dmg_left_4, dmg_center_4, _dmg_right_4, _dmg_low_4, _dmg_hi_4) =
            run_overheat_fit(4).await;

        // Rifter SDE: heatGenerationMultiplier (1224) = 1.0 at Thermo 0; 125mm AC II (2873) heatAbsorbtionRateModifier (1180) = 0.02
        assert!(hi_heat > 0.0, "High rack heat (attr 1175 heatHi) must accumulate during overloaded weapon cycles");
        // 5MN MWD II (440) heatAbsorbtionRateModifier (1180) = 0.04 -> rate = 1.0 * 0.04 = 0.04 over 80s
        let expected_med_heat = 1.0 - (-0.04_f64 * 80.0).exp();
        assert!(
            (med_heat - expected_med_heat).abs() < 1e-6,
            "Mid rack heat (attr 1176 heatMed) must match SDE formula 1 - exp(-1.0 * 0.04 * 80): got {med_heat}, expected {expected_med_heat}"
        );
        assert!(med_heat > hi_heat, "Higher heatAbsorbtionRateModifier (0.04 vs 0.02) must build more rack heat");
        assert_eq!(low_heat, 0.0, "Unheated low rack (attr 1177 heatLow) must remain 0.0");

        assert!(dmg_center_0 > 0.0, "Overloaded center MWD must take heat damage from seeded roll");
        assert!(
            dmg_left_0 > 0.0 || dmg_right_0 > 0.0,
            "Adjacent mid-rack modules must take propagated heat damage from seeded roll (left={dmg_left_0}, right={dmg_right_0})"
        );
        assert_eq!(dmg_low_0, 0.0, "Unheated low rack module must take 0 heat damage across rack boundary");

        // Live SystemActor Thermodynamics IV must reduce heat damage by 20% (0.8x of level 0)
        assert!(
            (dmg_center_4 - 0.8 * dmg_center_0).abs() < 1e-6,
            "Live SystemActor Thermodynamics IV must give 0.8x heat damage of level 0 (lvl4={dmg_center_4}, lvl0={dmg_center_0})"
        );
    }
}

pub(crate) mod skinr_sequencing_paragon_hub_listing {
    // Integration tests closing 17 `feat:cosmetics.*` and `feat:crimewatch.*` parity rows (Batch 23).
    use crate::crimewatch_timers_and_drones::{dict_get, item_rec, setup_pilot_in_space};
    use crate::quasar_goal_tale_notices::{bytes, sub, varint};
    use crate::quasar_progression_complete::{build_sess, quasar_rpc};
    use eve_db::{InventoryFlag, IskCents, TrainedSkillRecord};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
    use eve_proto::PyRep;
    use eve_server::combat::crimewatch::AggressionEvaluation;
    use eve_server::combat::npc_ai::compute_sec_status_gain;
    use eve_server::formulas::concord_response_delay_sec;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        check_crimewatch_action_restriction, evaluate_and_record_pvp_aggression,
        evaluate_and_record_remote_assistance, evaluate_faction_police_response,
        faction_police_sec_threshold, with_mut_combat_runtime, SafetyLevel,
    };
    use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;
    use glam::DVec3;
    use std::collections::HashMap;
    const JITA_SYS: u32 = 30_000_142; // 0.9
    const AMARR_SYS: u32 = 30_002_187; // 1.0
    const UEDAMA_SYS: u32 = 30_002_768; // 0.5
    const TAMA_SYS: u32 = 30_002_813; // 0.3 (low-sec)
    const HED_GP_SYS: u32 = 30_001_161; // -0.2 (null-sec)
    fn player_sess(sid: u64, cid: u32, corp: u32) -> SessionState {
        let mut s = build_sess(sid, cid, corp);
        s.user_id = Some(sid);
        s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        s
    }

    fn seed_market_char(ctx: &ServerContext, cid: i64, corp: u32, isk: i64, plex: i64) {
        ctx.db.with_mut_snapshot(|s| {
            let template = s.characters[&90_000_001].clone();
            let ch = s.characters.entry(cid).or_insert_with(|| template);
            ch.character_id = cid;
            ch.corporation_id = corp as i64;
            ch.balance = IskCents::from_isk(isk);
            ch.plex_balance = plex;
            ch.loyalty_points.insert(PARAGON_CORP_ID as i64, 100_000);
            ch.station_id = Some(60_003_760);
        });
    }

    #[tokio::test]
    async fn test_b23_skinr_sequencing_and_paragon_hub_listing() {
        let ctx = ServerContext::new();
        let seller_cid: u32 = 95_230_001;
        let buyer_cid: u32 = 95_230_002;
        let corp_id: u32 = 98_230_001;
        seed_market_char(&ctx, seller_cid as i64, corp_id, 50_000_000, 5_000);
        seed_market_char(&ctx, buyer_cid as i64, corp_id, 50_000_000, 5_000);
        ctx.db.with_mut_snapshot(|s| {
            let sk = s.skills.entry(seller_cid as i64).or_default();
            for &(tid, lvl) in &[
                (81377, 5),
                (81372, 5),
                (84217, 5),
                (84218, 5),
                (81368, 2),
            ] {
                sk.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: lvl,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
        let mut seller = player_sess(952_301, seller_cid, corp_id);
        let mut seller_gm = build_sess(952_309, seller_cid, corp_id);
        let mut buyer = player_sess(952_302, buyer_cid, corp_id);
        // 1. Check tier durations scaled by SDE Induction skills (30% discount)
        let rsp_dur = quasar_rpc(
            &ctx,
            &mut seller,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
            Vec::new(),
        ).await;
        assert_eq!(rsp_dur.status_code, STATUS_OK);
        let wm_dur = WireMessage::decode(&rsp_dur.payload.as_ref().unwrap().value).expect("decode dur");
        assert_eq!(varint(&wm_dur, 2), Some(30));
        // 2. Check Paragon Hub tax rate reduced by Paragon Relations + Paragon Partnership (20% - 10% = 10%)
        let rsp_tax = quasar_rpc(
            &ctx,
            &mut seller,
            "eve_public.cosmetic.market.api.GetTaxRateRequest",
            Vec::new(),
        ).await;
        assert_eq!(rsp_tax.status_code, STATUS_OK);
        let wm_tax = WireMessage::decode(&rsp_tax.payload.as_ref().unwrap().value).expect("decode tax");
        let tr_msg = sub(&wm_tax, 1);
        assert_eq!(varint(&tr_msg, 1), Some(20)); // sint64(10) zigzag-encoded = 20
        // 3. Grant unactivated custom SKIN license, list on Paragon Hub, and have buyer purchase it
        let skin_hex = "b2300001000000000000000000000001";
        let mut grant_req = Vec::new();
        let mut skin_id = Vec::new();
        encode_string_field(1, skin_hex, &mut skin_id);
        encode_bytes_field(1, &skin_id, &mut grant_req);
        encode_varint_field(2, 2, &mut grant_req);
        let rsp_grant = quasar_rpc(
            &ctx,
            &mut seller_gm,
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
            grant_req,
        ).await;
        assert_eq!(rsp_grant.status_code, STATUS_OK);
        let mut price_isk = Vec::new();
        encode_varint_field(1, 1_000_000 * 2, &mut price_isk); // sint64 zigzag of 1_000_000
        let mut lic_id_msg = Vec::new();
        let mut seller_seq = Vec::new();
        encode_varint_field(1, seller_cid as u64, &mut seller_seq);
        encode_bytes_field(1, &seller_seq, &mut lic_id_msg);
        encode_bytes_field(2, &skin_id, &mut lic_id_msg);
        let mut dur_msg = Vec::new();
        encode_varint_field(1, 86_400, &mut dur_msg);
        let mut target_msg = Vec::new();
        encode_varint_field(1, 1, &mut target_msg);
        let mut create_req = Vec::new();
        encode_bytes_field(1, &price_isk, &mut create_req);
        encode_bytes_field(3, &lic_id_msg, &mut create_req);
        encode_bytes_field(4, &dur_msg, &mut create_req);
        encode_varint_field(5, 1, &mut create_req);
        encode_bytes_field(6, &target_msg, &mut create_req);
        let rsp_create = quasar_rpc(
            &ctx,
            &mut seller,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            create_req,
        ).await;
        assert_eq!(rsp_create.status_code, STATUS_OK);
        let rsp_owned = quasar_rpc(
            &ctx,
            &mut seller,
            "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
            Vec::new(),
        ).await;
        assert_eq!(rsp_owned.status_code, STATUS_OK);
        let wm_owned = WireMessage::decode(&rsp_owned.payload.as_ref().unwrap().value).expect("decode owned");
        let owned_listing = sub(&wm_owned, 1);
        let listing_uuid = bytes(&sub(&owned_listing, 1), 1);
        let listing_price_isk = sub(&sub(&owned_listing, 2), 5);
        assert_eq!(varint(&listing_price_isk, 1), Some(1_000_000 * 2)); // sint64 ZigZag-encoded
        let seller_bal_before_sale = ctx
            .db
            .with_snapshot(|s| s.characters[&(seller_cid as i64)].balance.0);
        let mut uuid_msg = Vec::new();
        encode_bytes_field(1, &listing_uuid, &mut uuid_msg);
        let mut buy_req = Vec::new();
        encode_bytes_field(1, &uuid_msg, &mut buy_req);
        encode_varint_field(2, 1, &mut buy_req);
        let rsp_buy = quasar_rpc(
            &ctx,
            &mut buyer,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_req,
        ).await;
        assert_eq!(rsp_buy.status_code, STATUS_OK);
        let seller_bal_after_sale = ctx
            .db
            .with_snapshot(|s| s.characters[&(seller_cid as i64)].balance.0);
        assert_eq!(seller_bal_after_sale - seller_bal_before_sale, 90_000_000);
    }

    #[tokio::test]
    async fn test_b23_sec_status_caps_tags_outlaw_and_faction_police() {
        let ctx = ServerContext::new();
        let char_id: i64 = 95_230_010;
        let ship_id: i64 = 105_230_010;
        let pod_id: i64 = 105_230_011;
        let lowsec_sec_office_station = ctx
            .sde
            .stations
            .values()
            .find_map(|st| {
                let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
                let has_office = ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .is_some_and(|op| {
                        op.services.iter().any(|sid| {
                            ctx.sde.station_services.get(sid).is_some_and(|svc| {
                                svc.name.eq_ignore_ascii_case("Security Office")
                                    || svc.name.eq_ignore_ascii_case("securityOffice")
                            })
                        })
                    })
                    || st.owner_corporation_id == 1_000_125
                    || st.owner_corporation_id == 1_000_137;
                (0.0 < sec && sec < 0.45 && has_office).then_some(st.station_id)
            })
            .unwrap_or(60_014_861);
        let mut sess = setup_pilot_in_space(&ctx, 952_310, char_id, 98_230_002, ship_id, 621, JITA_SYS, DVec3::ZERO).await;
        sess.station_id = Some(lowsec_sec_office_station);
        // 1. Security status gain caps at +5.0 from NPC kills
        assert_eq!(compute_sec_status_gain(5.0, 1_000_000.0), 0.0);
        let near_cap = compute_sec_status_gain(4.9999, 1_000_000.0);
        assert!((4.9999 + near_cap) <= 5.0 + 1e-9);
        // 2. Clone soldier tag tier bands: Trainer (-10..-8), Recruiter (-8..-5), Transporter (-5..-2), Negotiator (-2..0)
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.station_id = Some(lowsec_sec_office_station);
            ch.balance = IskCents::from_isk(50_000_000);
            ch.security_status = -9.0;
            s.items.insert(
                700_001,
                item_rec(700_001, 33138, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
            );
            s.items.insert(
                700_002,
                item_rec(700_002, 33139, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
            );
            s.items.insert(
                700_003,
                item_rec(700_003, 33141, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
            );
        });
        let kw = HashMap::new();
        let bad_tag = ctx
            .dispatch_rpc(
                &mut sess,
                "crimewatch",
                None,
                "ApplyTagsForSecStatus",
                &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33139), PyRep::Integer(1))])],
                &kw,
                None,
            ).await;
        assert_eq!(bad_tag.unwrap_err().code, "SecurityTagBandMismatch");
        let ok_tag = ctx
            .dispatch_rpc(
                &mut sess,
                "crimewatch",
                None,
                "ApplyTagsForSecStatus",
                &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
                &kw,
                None,
            ).await
            .expect("Trainer tag accepted");
        assert_eq!(
            dict_get(&ok_tag, "newSecurityStatus").and_then(|v| v.as_f64()),
            Some(-8.5)
        );
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&char_id).unwrap().security_status = -0.2;
        });
        let cap_tag = ctx
            .dispatch_rpc(
                &mut sess,
                "crimewatch",
                None,
                "ApplyTagsForSecStatus",
                &[PyRep::Real(0.3), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
                &kw,
                None,
            ).await
            .expect("Negotiator tag caps at 0.0");
        assert!(
            dict_get(&cap_tag, "newSecurityStatus")
                .and_then(|v| v.as_f64())
                .unwrap()
                .abs()
                < 1e-6
        );
        // 3. Faction police thresholds per system security & pod vs ship
        assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
        assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
        assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
        assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
        assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
        assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&char_id).unwrap().security_status = -5.0;
            s.items.insert(
                pod_id,
                item_rec(pod_id, 670, char_id, JITA_SYS as i64, InventoryFlag::None, 1, true),
            );
        });
        let (engage_ship, allow_ship_dock, _) =
            evaluate_faction_police_response(&ctx, JITA_SYS, char_id, ship_id).await;
        assert!(engage_ship);
        assert!(!allow_ship_dock);
        let (engage_pod, allow_pod_dock, _) =
            evaluate_faction_police_response(&ctx, JITA_SYS, char_id, pod_id).await;
        assert!(!engage_pod);
        assert!(allow_pod_dock);
        // 4. Outlaw status (<= -4.95): legal to attack in high-sec without CONCORD
        let hunter_id: i64 = 95_230_011;
        let hunter_ship: i64 = 105_230_012;
        let _ = setup_pilot_in_space(
            &ctx,
            952_311,
            hunter_id,
            98_230_003,
            hunter_ship,
            621,
            JITA_SYS,
            DVec3::new(1000.0, 0.0, 0.0),
        ).await;
        evaluate_and_record_pvp_aggression(&ctx, hunter_id, hunter_ship, JITA_SYS, ship_id, false)
            .expect("attacking outlaw is legal");
        let now = ctx.now_unix();
        assert!(!ctx.crimewatch.write().unwrap().get_or_create(hunter_id).is_criminal(now));
    }

    #[tokio::test]
    async fn test_b23_weapon_timer_scope_and_pvp_logoff_renewal() {
        let ctx = ServerContext::new();
        let char_id: i64 = 95_230_020;
        let ship_id: i64 = 105_230_020;
        let mut sess = setup_pilot_in_space(&ctx, 952_320, char_id, 98_230_004, ship_id, 621, UEDAMA_SYS, DVec3::ZERO).await;
        let now = ctx.now_unix();
        ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now);
        for act in [
            "stargate_jump",
            "dock",
            "acceleration_gate",
            "eject",
            "store_ship",
            "refit",
            "switch_ship",
            "tether",
        ] {
            assert!(
                check_crimewatch_action_restriction(&ctx, char_id, ship_id, UEDAMA_SYS, act, now)
                    .is_err(),
                "weapon timer must block {act}"
            );
        }
        // Disconnected ship PvP logoff timer renewal vs NPC timer non-renewal
        let victim_id: i64 = 95_230_021;
        let _ = setup_pilot_in_space(
            &ctx,
            952_321,
            victim_id,
            98_230_005,
            105_230_021,
            621,
            UEDAMA_SYS,
            DVec3::new(500.0, 0.0, 0.0),
        ).await;
        let disc = eve_server::session_cluster_lifecycle::handle_character_logoff_or_disconnect(&ctx, victim_id).await
            .expect("disconnect in space");
        let initial_rem = disc.remove_at_unix;

        eve_server::session_cluster_lifecycle::record_npc_aggression_timer(&ctx, victim_id, now + 10);
        let rem_after_npc = eve_server::session_cluster_lifecycle::with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships[&victim_id].remove_at_unix
        });
        assert_eq!(rem_after_npc, initial_rem);

        eve_server::session_cluster_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
            &ctx,
            char_id,
            victim_id,
            now + 500,
        );
        let rem_after_pvp = eve_server::session_cluster_lifecycle::with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships[&victim_id].remove_at_unix
        });
        assert_eq!(rem_after_pvp, now + 1_400);

        // PvP timer blocks Abyssal filament activation
        let kw = HashMap::new();
        let fil_err = ctx
            .dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEntranceDeployment",
                &[PyRep::Integer(47_765)],
                &kw,
                None,
            ).await;
        assert_eq!(fil_err.unwrap_err().code, "PvpTimerActive");
    }

    #[tokio::test]
    async fn test_b23_concord_response_scaling_and_highsec_criminal_restrictions() {
        let ctx = ServerContext::new();
        let _ = concord_response_delay_sec(0.5);
        for (sec, expected_delay) in [
            (0.5, 19.0),
            (0.6, 14.0),
            (0.7, 11.0),
            (0.8, 8.0),
            (0.9, 6.0),
            (1.0, 6.0),
        ] {
            let mut cw = eve_server::combat::crimewatch::CrimeWatchDirector::new();
            let ev = cw.handle_combat_aggression_with_pod(1, 2, 10, JITA_SYS, sec, false, false, 100);
            assert_eq!(
                ev,
                AggressionEvaluation::CriminalFlagTriggered {
                    concord_delay_sec: expected_delay,
                    arrival_unix: 100 + expected_delay as u64,
                }
            );
        }

        let crim_id: i64 = 95_230_030;
        let crim_ship: i64 = 105_230_030;
        let vic_id: i64 = 95_230_031;
        let vic_ship: i64 = 105_230_031;
        let _ = setup_pilot_in_space(&ctx, 952_330, crim_id, 98_230_006, crim_ship, 621, JITA_SYS, DVec3::ZERO).await;
        let _ = setup_pilot_in_space(
            &ctx,
            952_331,
            vic_id,
            98_230_007,
            vic_ship,
            621,
            JITA_SYS,
            DVec3::new(1000.0, 0.0, 0.0),
        ).await;

        let now = ctx.now_unix();
        let eval = ctx
            .crimewatch
            .write()
            .unwrap()
            .handle_combat_aggression_with_pod(
                crim_id,
                vic_id,
                crim_ship,
                JITA_SYS,
                0.9,
                false,
                false,
                now,
            );
        assert_eq!(
            eval,
            AggressionEvaluation::CriminalFlagTriggered {
                concord_delay_sec: 6.0,
                arrival_unix: now + 6,
            }
        );

        for act in [
            "warp",
            "stargate_jump",
            "dock",
            "eject",
            "store_ship",
            "switch_ship",
        ] {
            assert!(
                check_crimewatch_action_restriction(&ctx, crim_id, crim_ship, JITA_SYS, act, now)
                    .is_err(),
                "high-sec criminal must block {act}"
            );
        }

        let mut cw_director = eve_server::combat::crimewatch::CrimeWatchDirector::new();
        let _ = cw_director.handle_combat_aggression_with_pod(
            crim_id,
            vic_id,
            crim_ship,
            JITA_SYS,
            0.9,
            false,
            false,
            now,
        );
        let reports = cw_director.step_concord(&ctx, now + 1_000, 42).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].webbed_speed_fraction, 0.0);
        assert_eq!(reports[0].warp_scramble_strength, 100);
        assert_eq!(reports[0].capacitor_after_neut, 0.0);
        assert!(reports[0].ecm_jammed);
        assert!(reports[0].death_result.is_ok());

        let low_att: i64 = 95_230_032;
        let low_vic: i64 = 95_230_033;
        let e_ship = ctx
            .crimewatch
            .write()
            .unwrap()
            .handle_combat_aggression_with_pod(
                low_att,
                low_vic,
                105_230_032,
                TAMA_SYS,
                0.3,
                false,
                false,
                now,
            );
        assert_eq!(e_ship, AggressionEvaluation::SuspectFlagTriggered);

        let e_pod = ctx
            .crimewatch
            .write()
            .unwrap()
            .handle_combat_aggression_with_pod(
                low_att,
                low_vic,
                105_230_032,
                TAMA_SYS,
                0.3,
                false,
                true,
                now,
            );
        assert_eq!(
            e_pod,
            AggressionEvaluation::CriminalFlagTriggered {
                concord_delay_sec: 0.0,
                arrival_unix: now,
            }
        );
    }

    #[tokio::test]
    async fn test_b23_suspect_looting_ess_and_criminal_wreck_auto_abandon() {
        let ctx = ServerContext::new();
        let thief_id: i64 = 95_230_040;
        let thief_ship: i64 = 105_230_040;
        let owner_id: i64 = 95_230_041;
        let mut thief_sess = setup_pilot_in_space(&ctx, 952_340, thief_id, 98_230_008, thief_ship, 621, HED_GP_SYS, DVec3::ZERO).await;
        let _ = setup_pilot_in_space(&ctx, 952_341, owner_id, 98_230_009, 105_230_041, 621, HED_GP_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;
        let ess_ball_id: i64 = 700_099;
        let _ = ctx.systems.get_or_spawn(HED_GP_SYS).spawn_ball(eve_destiny::Ball {
            id: ess_ball_id,
            type_id: 32882,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;

        ctx.db.with_mut_snapshot(|s| {
            s.system_states.entry(HED_GP_SYS).or_default().ess_main_bank = IskCents::from_isk(10_000_000);
        });
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(
                &mut thief_sess,
                "essMgr",
                None,
                "AttemptLinkToMainBank",
                &[PyRep::Integer(ess_ball_id)],
                &kw,
                None,
            ).await
            .expect("AttemptLinkToMainBank");
        let now = ctx.now_unix();
        assert!(ctx.crimewatch.read().unwrap().is_suspect(thief_id, now));

        let looter_id: i64 = 95_230_042;
        let looter_ship: i64 = 105_230_042;
        let mut looter_sess = setup_pilot_in_space(&ctx, 952_342, looter_id, 98_230_010, looter_ship, 621, JITA_SYS, DVec3::ZERO).await;
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(looter_id, SafetyLevel::Green);
        });
        ctx.crimewatch
            .write()
            .unwrap()
            .get_or_create(owner_id)
            .criminal_flag_until_unix = now + 900;
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&owner_id).unwrap().solar_system_id = JITA_SYS;
            s.items.insert(
                700_040,
                item_rec(700_040, 23, owner_id, JITA_SYS as i64, InventoryFlag::None, 1, true),
            );
            s.items.insert(
                700_041,
                item_rec(700_041, 34, owner_id, 700_040, InventoryFlag::Cargo, 100, false),
            );
        });
        let loot_res = ctx
            .dispatch_rpc(
                &mut looter_sess,
                "invbroker",
                None,
                "Add",
                &[
                    PyRep::Integer(700_041),
                    PyRep::Integer(700_040),
                    PyRep::Integer(InventoryFlag::Cargo as i64),
                    PyRep::Integer(50),
                    PyRep::Integer(looter_ship),
                ],
                &kw,
                None,
            ).await;
        assert!(loot_res.is_ok(), "criminal jetcan is auto-abandoned and lootable under Green safety");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&700_041].quantity), 50);
        assert!(!ctx.crimewatch.read().unwrap().is_suspect(looter_id, now));
    }

    #[tokio::test]
    async fn test_b23_limited_engagement_duel_safety_and_third_party_assistance() {
        let ctx = ServerContext::new();
        let p1_id: i64 = 95_230_050;
        let p1_ship: i64 = 105_230_050;
        let p2_id: i64 = 95_230_051;
        let p2_ship: i64 = 105_230_051;
        let logi_id: i64 = 95_230_052;
        let logi_ship: i64 = 105_230_052;

        let mut p1_sess = setup_pilot_in_space(&ctx, 952_350, p1_id, 98_230_011, p1_ship, 621, AMARR_SYS, DVec3::ZERO).await;
        let mut p2_sess = setup_pilot_in_space(
            &ctx,
            952_351,
            p2_id,
            98_230_012,
            p2_ship,
            621,
            AMARR_SYS,
            DVec3::new(1000.0, 0.0, 0.0),
        ).await;
        let _ = setup_pilot_in_space(
            &ctx,
            952_352,
            logi_id,
            98_230_013,
            logi_ship,
            621,
            AMARR_SYS,
            DVec3::new(1500.0, 0.0, 0.0),
        ).await;

        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(
                &mut p1_sess,
                "crimewatch",
                None,
                "StartDuelChallenge",
                &[PyRep::Integer(p2_id)],
                &kw,
                None,
            ).await
            .expect("StartDuelChallenge");
        let resp = ctx
            .dispatch_rpc(
                &mut p2_sess,
                "crimewatch",
                None,
                "RespondToDuelChallenge",
                &[PyRep::Integer(p1_id), PyRep::Bool(true)],
                &kw,
                None,
            ).await
            .expect("RespondToDuelChallenge");
        assert_eq!(
            dict_get(&resp, "limitedEngagementSeconds").and_then(|v| v.as_i64()),
            Some(300)
        );

        evaluate_and_record_pvp_aggression(&ctx, p1_id, p1_ship, AMARR_SYS, p2_ship, false)
            .expect("duel combat is legal");
        let now = ctx.now_unix();
        assert!(!ctx.crimewatch.write().unwrap().get_or_create(p1_id).is_criminal(now));

        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(logi_id, SafetyLevel::Green);
        });
        let green_err = evaluate_and_record_remote_assistance(&ctx, logi_id, logi_ship, AMARR_SYS, p1_ship);
        assert_eq!(green_err.unwrap_err().code, "CantDoInCurrentSafetyLevel");

        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(logi_id, SafetyLevel::Red);
        });
        evaluate_and_record_remote_assistance(&ctx, logi_id, logi_ship, AMARR_SYS, p1_ship)
            .expect("Red safety allows remote assistance");
        assert!(ctx.crimewatch.write().unwrap().get_or_create(logi_id).is_criminal(now));
    }
}

pub(crate) mod abyssal_suspect_flag_on_exit {
    // Integration tests for Batch 23 behavioral verification (Abyssal exit suspect flag, war interference, command burst filter, CONCORD & jump-in).

    use crate::crimewatch_timers_and_drones::{dict_get, item_rec, setup_pilot_in_space};
    use eve_db::{InventoryFlag, WarRecord};
    use eve_proto::PyRep;
    use eve_server::combat::crimewatch::concord_delay_for_security;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        evaluate_and_record_pvp_aggression, evaluate_and_record_remote_assistance,
        with_mut_combat_runtime, SafetyLevel,
    };
    use eve_server::services::pve_abyssal_lifecycle::evacuate_abyssal_pockets_for_downtime;
    use eve_server::services::tactical_modules::activate_command_burst;
    use glam::DVec3;
    use std::collections::HashMap;

    const AMARR_SYS: u32 = 30_002_187; // 1.0
    const TAMA_SYS: u32 = 30_002_813; // 0.3

    #[tokio::test]
    async fn test_b23_abyssal_suspect_flag_on_exit_not_entry() {
        let ctx = ServerContext::new();
        let char_id: i64 = 95_230_101;
        let ship_id: i64 = 105_230_101;
        let sys_07 = ctx
            .sde
            .solar_systems
            .iter()
            .find_map(|(&sid, s)| (0.65..0.75).contains(&s.security_status).then_some(sid))
            .expect("0.7 high-sec system in SDE");
        let mut sess = setup_pilot_in_space(&ctx, 952_401, char_id, 98_230_101, ship_id, 621, sys_07, DVec3::ZERO).await;

        let t6_filament_tid = ctx
            .sde
            .types
            .keys()
            .copied()
            .filter(|&tid| {
                eve_server::services::pve_special_sites_abyssal::classify_abyssal_filament(tid)
                    .is_some_and(|(tier, _)| tier == 6)
            })
            .min()
            .unwrap_or(56_139);

        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                700_101,
                item_rec(700_101, t6_filament_tid, char_id, ship_id, InventoryFlag::Cargo, 1, false),
            );
        });

        let kw = HashMap::new();
        let deploy_res = ctx
            .dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEntranceDeployment",
                &[PyRep::Integer(700_101)],
                &kw,
                None,
            )
            .await
            .expect("AbyssalEntranceDeployment");
        let trace_ball_id = dict_get(&deploy_res, "traceBallID")
            .and_then(|v| v.as_i64())
            .expect("traceBallID");
        let _enter_res = ctx
            .dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEntranceGateActivation",
                &[PyRep::Integer(trace_ball_id)],
                &kw,
                None,
            )
            .await
            .expect("AbyssalEntranceGateActivation");

        let now = ctx.now_unix();
        assert!(
            !ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(now),
            "T6 Abyssal filament MUST NOT set suspect flag on entry in 0.7 high-sec"
        );

        ctx.advance_time_seconds(120);
        let exit_now = ctx.now_unix();
        let evacuated = evacuate_abyssal_pockets_for_downtime(&ctx);
        assert_eq!(evacuated, 1);
        assert!(
            ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(exit_now),
            "T6 Abyssal filament MUST set suspect flag on exit from 0.7 high-sec"
        );
    }

    #[tokio::test]
    async fn test_b23_war_interference_and_command_burst_filtering() {
        let ctx = ServerContext::new();
        let corp_a: i64 = 98_230_201;
        let corp_b: i64 = 98_230_202;
        let corp_c: i64 = 98_230_203;

        let pilot_a: i64 = 95_230_201;
        let ship_a: i64 = 105_230_201;
        let corpmate_a: i64 = 95_230_202;
        let ship_mate: i64 = 105_230_202;
        let neutral_logi: i64 = 95_230_203;
        let ship_logi: i64 = 105_230_203;

        let mut sess_a = setup_pilot_in_space(&ctx, 952_411, pilot_a, corp_a, ship_a, 621, AMARR_SYS, DVec3::ZERO).await;
        let mut sess_mate = setup_pilot_in_space(&ctx, 952_412, corpmate_a, corp_a, ship_mate, 621, AMARR_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
        let mut sess_logi = setup_pilot_in_space(&ctx, 952_413, neutral_logi, corp_c, ship_logi, 621, AMARR_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;

        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|s| {
            s.wars.insert(
                990_001,
                WarRecord {
                    war_id: 990_001,
                    aggressor_corp_id: corp_a,
                    defender_corp_id: corp_b,
                    mutual: true,
                    started_unix: now.saturating_sub(100_000),
                    active: true,
                },
            );
        });

        // Corpmate assisting war participant is legal
        evaluate_and_record_remote_assistance(&ctx, corpmate_a, ship_mate, AMARR_SYS, ship_a)
            .expect("corpmate assistance is legal");
        assert!(!ctx.crimewatch.write().unwrap().get_or_create(corpmate_a).is_criminal(now));

        // Negative: Neutral third party assisting an IDLE war member (no PvP timer) -> no flag
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(neutral_logi, SafetyLevel::Green);
        });
        evaluate_and_record_remote_assistance(&ctx, neutral_logi, ship_logi, AMARR_SYS, ship_a)
            .expect("assisting idle war member with no PvP timer is legal even on Green safety");
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(neutral_logi);
            assert!(!st.is_criminal(now));
            assert!(!st.is_suspect(now));
        }

        // Activate PvP timer on pilot_a (war engagement)
        ctx.crimewatch.write().unwrap().get_or_create(pilot_a).pvp_timer_until_unix = now + 900;

        // Neutral third party assisting war participant with active PvP timer in high-sec triggers Criminal + CONCORD + notification
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(neutral_logi, SafetyLevel::Red);
        });
        let _ = ctx.drain_notifications_for_char(neutral_logi);
        evaluate_and_record_remote_assistance(&ctx, neutral_logi, ship_logi, AMARR_SYS, ship_a)
            .expect("Red safety allows war interference");
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(neutral_logi);
            assert!(st.is_criminal(now));
            assert!(st.pending_concord_arrival.is_some());
        }
        let notes = ctx.drain_notifications_for_char(neutral_logi);
        assert!(notes.iter().any(|n| n.event == "OnCriminalTimerUpdate"));

        // Low-sec war interference triggers Suspect (not Criminal)
        let logi_low: i64 = 95_230_204;
        let ship_logi_low: i64 = 105_230_204;
        let _ = setup_pilot_in_space(&ctx, 952_414, logi_low, corp_c, ship_logi_low, 621, TAMA_SYS, DVec3::ZERO).await;
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(logi_low, SafetyLevel::Red);
        });
        evaluate_and_record_remote_assistance(&ctx, logi_low, ship_logi_low, TAMA_SYS, ship_a)
            .expect("low-sec war interference");
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(logi_low);
            assert!(st.is_suspect(now));
            assert!(!st.is_criminal(now));
        }

        // Command burst in high-sec: corpmate receives burst, neutral booster excludes war-engaged pilot
        let kw = HashMap::new();
        let _ = ctx.dispatch_rpc(&mut sess_a, "fleetMgr", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
        let fleet_id = sess_a.fleet_id.expect("fleet_id");
        let _ = ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(corpmate_a), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect("Invite mate");
        let _ = ctx.dispatch_rpc(&mut sess_mate, "fleetMgr", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None).await.expect("AcceptInvite mate");
        let _ = ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(neutral_logi), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect("Invite logi");
        let _ = ctx.dispatch_rpc(&mut sess_logi, "fleetMgr", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None).await.expect("AcceptInvite logi");

        let (burst_mod_tid, burst_charge_tid) = ctx
            .sde
            .types
            .iter()
            .find_map(|(&tid, t)| {
                if t.group_id != 1770 || t.name.contains(" II") || t.name.contains("Presidential") || t.name.contains("Expedition") || t.name.contains("Mining Foreman") {
                    return None;
                }
                let cg = t.default_attributes.get(&604).copied()? as u32;
                let c_tid = ctx.sde.types.iter().find_map(|(&ctid, ct)| (ct.group_id == cg).then_some(ctid))?;
                Some((tid, c_tid))
            })
            .expect("Tech I Command Burst module and charge in SDE");
        let burst_mod_id: i64 = 700_201;
        let burst_charge_id: i64 = 700_202;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(sh) = s.items.get_mut(&ship_a) {
                sh.type_id = 37480;
            }
            s.items.insert(burst_mod_id, item_rec(burst_mod_id, burst_mod_tid, pilot_a, ship_a, InventoryFlag::HiSlot0, 1, true));
            s.items.insert(burst_charge_id, item_rec(burst_charge_id, burst_charge_tid, pilot_a, ship_a, InventoryFlag::HiSlot0, 10, false));
            s.module_online_states.insert(burst_mod_id, true);
        });
        ctx.invalidate_ship_fitting(ship_a);
        let sys_h = ctx.systems.get_or_spawn(AMARR_SYS);
        let fit_a = ctx.get_or_rebuild_ship_fitting(pilot_a, ship_a).expect("fit_a");
        let mod_type = ctx.sde.types.get(&burst_mod_tid).expect("burst_mod_tid");
        let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, burst_mod_tid, &ctx.sde);
        let burst_res = activate_command_burst(&ctx, &sys_h, pilot_a, ship_a, AMARR_SYS, burst_mod_id, mod_type, &mod_state, Some(burst_charge_tid), &fit_a)
            .await
            .expect("corpmate burst");
        let affected: Vec<i64> = match dict_get(&burst_res, "affectedMemberIDs") {
            Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        assert!(affected.contains(&corpmate_a), "corpmate in same war must receive command burst");
        assert!(!affected.contains(&neutral_logi), "neutral pilot who is criminal must be excluded");
        assert!(
            ctx.crimewatch.read().unwrap().states.get(&pilot_a).is_some_and(|s| s.weapon_timer_until_unix > ctx.now_unix()),
            "Command burst must apply mandatory 60s Weapons Timer"
        );

        // Clear neutral_logi's criminal flag so only war-interference exclusion is tested
        ctx.crimewatch.write().unwrap().states.remove(&neutral_logi);
        let burst_mod_id2: i64 = 700_203;
        let burst_charge_id2: i64 = 700_204;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(sh) = s.items.get_mut(&ship_logi) {
                sh.type_id = 37480;
            }
            s.items.insert(burst_mod_id2, item_rec(burst_mod_id2, burst_mod_tid, neutral_logi, ship_logi, InventoryFlag::HiSlot0, 1, true));
            s.items.insert(burst_charge_id2, item_rec(burst_charge_id2, burst_charge_tid, neutral_logi, ship_logi, InventoryFlag::HiSlot0, 10, false));
            s.module_online_states.insert(burst_mod_id2, true);
        });
        ctx.invalidate_ship_fitting(ship_logi);
        let fit_logi = ctx.get_or_rebuild_ship_fitting(neutral_logi, ship_logi).expect("fit_logi");
        let mod_state2 = eve_dogma::DogmaItemState::from_sde(burst_mod_id2, burst_mod_tid, &ctx.sde);
        let burst_res2 = activate_command_burst(&ctx, &sys_h, neutral_logi, ship_logi, AMARR_SYS, burst_mod_id2, mod_type, &mod_state2, Some(burst_charge_tid), &fit_logi)
            .await
            .expect("neutral burst");
        let affected2: Vec<i64> = match dict_get(&burst_res2, "affectedMemberIDs") {
            Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        assert!(!affected2.contains(&pilot_a), "war-engaged pilot must be excluded from neutral booster's command burst in high-sec");
    }

    #[tokio::test]
    async fn test_b23_le_interference_concord_and_jump_into_highsec_concord() {
        let ctx = ServerContext::new();
        let p1: i64 = 95_230_301;
        let s1: i64 = 105_230_301;
        let p2: i64 = 95_230_302;
        let s2: i64 = 105_230_302;
        let logi: i64 = 95_230_303;
        let s_logi: i64 = 105_230_303;

        let mut sess1 = setup_pilot_in_space(&ctx, 952_421, p1, 98_230_301, s1, 621, AMARR_SYS, DVec3::ZERO).await;
        let mut sess2 = setup_pilot_in_space(&ctx, 952_422, p2, 98_230_302, s2, 621, AMARR_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
        let _ = setup_pilot_in_space(&ctx, 952_423, logi, 98_230_303, s_logi, 621, AMARR_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;

        let kw = HashMap::new();
        let _ = ctx.dispatch_rpc(&mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(p2)], &kw, None).await.expect("StartDuelChallenge");
        let _ = ctx.dispatch_rpc(&mut sess2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(p1), PyRep::Bool(true)], &kw, None).await.expect("RespondToDuelChallenge");
        evaluate_and_record_pvp_aggression(&ctx, p1, s1, AMARR_SYS, s2, false).expect("duel legal");

        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(logi, SafetyLevel::Red);
        });
        let _ = ctx.drain_notifications_for_char(logi);
        evaluate_and_record_remote_assistance(&ctx, logi, s_logi, AMARR_SYS, s1).expect("LE interference");
        let now = ctx.now_unix();
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(logi);
            assert!(st.is_criminal(now));
            assert!(st.pending_concord_arrival.is_some(), "LE interference in high-sec must schedule CONCORD");
        }
        let notes = ctx.drain_notifications_for_char(logi);
        assert!(notes.iter().any(|n| n.event == "OnCriminalTimerUpdate"), "LE interference must emit timer notifications");

        // Stargate jump from low-sec into high-sec with active criminal timer schedules CONCORD
        let (gate_id, from_sys, to_sys, to_sec) = ctx
            .sde
            .jumps
            .iter()
            .find_map(|(&gid, j)| {
                let f_sec = ctx.sde.solar_systems.get(&j.from_solar_system_id)?.security_status;
                let t_sec = ctx.sde.solar_systems.get(&j.to_solar_system_id)?.security_status;
                (f_sec > 0.0 && f_sec < 0.45 && t_sec >= 0.45).then_some((gid, j.from_solar_system_id, j.to_solar_system_id, t_sec))
            })
            .expect("low-sec to high-sec stargate in SDE");

        let jumper: i64 = 95_230_304;
        let jumper_ship: i64 = 105_230_304;
        let gate_pos = ctx.sde.jumps[&gate_id].position;
        let mut j_sess = setup_pilot_in_space(&ctx, 952_424, jumper, 98_230_304, jumper_ship, 621, from_sys, gate_pos).await;
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(jumper);
            st.criminal_flag_until_unix = now + 900;
            st.pending_concord_arrival = None;
        }
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, jumper);
        let beyonce = ctx.bind_object(&mut j_sess, "beyonce", from_sys as i64).expect("bind beyonce");
        ctx.dispatch_rpc(&mut j_sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(gate_id as i64)], &kw, None)
            .await
            .expect("CmdStargateJump into high-sec");

        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(jumper);
        let (arr_sys, arr_unix, arr_ship, _) = st.pending_concord_arrival.expect("CONCORD scheduled on jump into high-sec");
        assert_eq!(arr_sys, to_sys);
        assert_eq!(arr_ship, jumper_ship);
        let expected_delay = concord_delay_for_security(to_sec).ceil() as u64;
        assert!(arr_unix >= now + expected_delay && arr_unix <= now + expected_delay + 2);
    }

    #[tokio::test]
    async fn test_skinr_plex_pricing_and_sec_office_tag_exchange() {
        use crate::quasar_progression_complete::{build_sess, quasar_rpc};
        use eve_db::{IskCents, TrainedSkillRecord};
        use eve_server::quasar_gateway::STATUS_OK;

        let ctx = ServerContext::new();
        let cid: u32 = 95_230_401;
        let corp_id: u32 = 98_230_401;
        ctx.db.with_mut_snapshot(|s| {
            let mut ch = s.characters[&90_000_001].clone();
            ch.character_id = cid as i64;
            ch.corporation_id = corp_id as i64;
            ch.plex_balance = 100;
            ch.balance = IskCents::from_isk(50_000_000);
            s.characters.insert(cid as i64, ch);
        });
        let mut sess = build_sess(952_430, cid, corp_id);
        sess.user_id = Some(952_430);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

        // 1. R-B23-01: SKINR InitiateRequest rejects empty payload and debits whole PLEX (17 PLEX for Tier 1, not 1,700)
        let empty_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(empty_rsp.status_code, 400, "Empty InitiateRequest must be rejected");

        use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
        let mut layout = Vec::new();
        let mut r_tid = Vec::new();
        encode_varint_field(1, 587, &mut r_tid);
        encode_bytes_field(2, &r_tid, &mut layout);
        encode_varint_field(3, 1, &mut layout);
        let mut init_req = Vec::new();
        encode_string_field(1, "B23 Test SKIN", &mut init_req);
        encode_string_field(2, "B23 Line", &mut init_req);
        encode_bytes_field(3, &layout, &mut init_req);
        encode_varint_field(5, 1, &mut init_req);

        let init_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req.clone(),
        )
        .await;
        assert_eq!(init_rsp.status_code, STATUS_OK);
        let plex_after = ctx.db.with_snapshot(|s| s.characters[&(cid as i64)].plex_balance);
        assert_eq!(plex_after, 83, "Tier 1 SKINR sequencing must debit 17 whole PLEX (100 - 17 = 83)");

        // With skill 81044 level 5 (10% price discount): 1700 * 90 / 100 = 1530 cents -> 16 whole PLEX
        ctx.db.with_mut_snapshot(|s| {
            s.skills.entry(cid as i64).or_default().insert(
                81044,
                TrainedSkillRecord { type_id: 81044, level: 5, skill_points: 256_000.0 },
            );
        });
        let init_rsp2 = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req,
        )
        .await;
        assert_eq!(init_rsp2.status_code, STATUS_OK);
        let plex_after2 = ctx.db.with_snapshot(|s| s.characters[&(cid as i64)].plex_balance);
        assert_eq!(plex_after2, 67, "10% discounted Tier 1 SKINR sequencing must debit 16 whole PLEX (83 - 16 = 67)");

        // 2. R-B23-04: ApplyTagsForSecStatus rejects High-Sec Jita & qty <= 0, accepts SDE 33141 at Security Office, charges ISK, records journal, crosses 0.0, and preserves excess tags
        let kw = HashMap::new();
        sess.station_id = Some(60_003_760);
        let jita_err = ctx
            .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])], &kw, None)
            .await
            .expect_err("High-Sec Jita must be rejected for Security Office tag turn-in");
        assert_eq!(jita_err.code, "SecurityOfficeRequired");

        let sec_office_station = ctx
            .sde
            .stations
            .iter()
            .find_map(|(&sid, st)| {
                let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
                ((0.0 < sec && sec < 0.45)
                    && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, sid, 27))
                .then_some(sid)
            })
            .expect("Low-sec station with CONCORD/DED Security Office service (27) in SDE");
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
                ch.station_id = Some(sec_office_station);
            }
        });
        sess.station_id = Some(sec_office_station);

        let zero_err = ctx
            .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(0))])], &kw, None)
            .await
            .expect_err("Zero quantity must be rejected");
        assert_eq!(zero_err.code, "InvalidQuantity");

        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let uid = sess.user_id.unwrap();
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&(cid as i64)).unwrap();
            ch.user_id = uid;
            ch.security_status = -0.1;
            ch.balance = eve_db::IskCents::from_isk(25_000_000);
            s.items.insert(700_998, item_rec(700_998, 33141, cid as i64, sec_office_station, InventoryFlag::Hangar, 1, false));
            s.items.insert(700_999, item_rec(700_999, 33141, cid as i64, sec_office_station, InventoryFlag::Hangar, 2, false));
        });
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid as i64)], &kw, None).await.unwrap();
        sess.character_id = Some(cid as i64);
        sess.station_id = Some(sec_office_station);
        let low_err = ctx
            .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(-0.5), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])], &kw, None)
            .await
            .expect_err("targetSecStatus <= cur_sec must be rejected");
        assert_eq!(low_err.code, "InvalidTargetSecurityStatus");
        let _ = ctx.drain_notifications_for_char(cid as i64);
        ctx.dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(3))])], &kw, None)
            .await
            .expect("ApplyTagsForSecStatus with SDE Clone Soldier Negotiator Tag (33141)");

        let notifs = ctx.drain_notifications_for_char(cid as i64);
        let item_change = notifs.iter().find(|n| n.event == "OnItemChange").expect("3-arg OnItemChange");
        assert_eq!(item_change.args.len(), 3, "OnItemChange must have 3 positional args");
        assert_eq!(item_change.args[2].as_i64(), Some(sec_office_station));

        ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&(cid as i64)];
            assert!((ch.security_status - 0.0).abs() < 1e-6, "Clone Soldier tags cap security status at 0.0, got {}", ch.security_status);
            assert_eq!(ch.balance, eve_db::IskCents::from_isk(15_000_000), "Must debit 10M ISK for 1 tag");
            assert_eq!(s.items.get(&700_999).map(|i| i.quantity), Some(2), "Must preserve 2 excess tags");
            assert!(
                s.wallet_journal.iter().any(|e| e.character_id == (cid as i64) && e.entry_type == "SecurityTagProcessing"),
                "Must record SecurityTagProcessing wallet journal entry"
            );
        });
    }
}

pub(crate) mod corp_governance_durability_persistence {
    // End-to-end integration tests for Batch 29 open `feat:lifecycle.*` parity rows.


    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use crate::common;
    use crate::crimewatch_timers_and_drones::{far_celestial_id, item_rec, setup_pilot_in_space};
    use eve_db::{
        InventoryFlag, IskCents, TrainedSkillRecord, VirtualServerClock, WorldRepository,
    };
    use eve_destiny::MovementMode;
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::fleet_social::fleet_social_notify::{
        handle_fleet_member_disconnect, tick_fleet_lifecycle, FLEET_DISCONNECT_GRACE_SECS,
        FLEET_INVITE_EXPIRY_SECS,
    };
    use glam::DVec3;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const PILOT_3: i64 = 90_000_003;
    const PILOT_4: i64 = 90_000_004;

    fn temp_sqlite_path(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("evomu_b29_{tag}_{pid}_{seq}.sqlite"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_ctx(db_path: &Path, now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
        let exists = db_path.exists();
        let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
        if !exists {
            let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            repo.with_mut_snapshot(|snap| *snap = seed_snap);
        }
        let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
        repo.set_clock(clock.clone());
        let ctx = ServerContext::with_repository(repo);
        ctx.set_clock(clock.clone());
        (ctx, clock)
    }

    async fn login_select(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
        sess
    }

    async fn rpc_res(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> PyRep {
        rpc_res(ctx, sess, svc, method, args)
            .await
            .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
    }

    fn pyrep_contains_int(rep: &PyRep, target: i64) -> bool {
        match rep {
            PyRep::Integer(v) => *v == target,
            PyRep::Tuple(items) | PyRep::List(items) => {
                items.iter().any(|x| pyrep_contains_int(x, target))
            }
            PyRep::Dict(entries) => entries
                .iter()
                .any(|(k, v)| pyrep_contains_int(k, target) || pyrep_contains_int(v, target)),
            PyRep::KeyVal(entries) => entries.iter().any(|(_, v)| pyrep_contains_int(v, target)),
            PyRep::Rowset { rows, .. } => rows
                .iter()
                .any(|row| row.iter().any(|x| pyrep_contains_int(x, target))),
            PyRep::Object { args, .. } => pyrep_contains_int(args, target),
            _ => false,
        }
    }

    fn session_changed_has_field(notifs: &[eve_server::router::PendingMachoNotification], field: &str, expected: i64) -> bool {
        notifs.iter().filter(|n| n.event == "OnSessionChanged").any(|n| {
            n.args.iter().any(|arg| match arg {
                PyRep::Dict(entries) => entries.iter().any(|(k, v)| {
                    k.as_str() == Some(field)
                        && matches!(v, PyRep::Tuple(pair) if pair.get(1) == Some(&PyRep::Integer(expected)))
                }),
                PyRep::KeyVal(entries) => entries.get(field).is_some_and(|v| {
                    matches!(v, PyRep::Tuple(pair) if pair.get(1) == Some(&PyRep::Integer(expected)))
                }),
                _ => false,
            })
        })
    }

    #[tokio::test]
    async fn test_b29_corp_governance_durability_and_persistence() {
        // parity: feat:lifecycle.corp-governance-durable
        // parity: feat:lifecycle.corp-governance-persist
        eve_testkit::attest(&[
            "feat:lifecycle.corp-governance-durable",
            "feat:lifecycle.corp-governance-persist",
        ]);

        let db_path = temp_sqlite_path("corp_gov");
        let t0 = 1_775_000_000u64;
        let hangar_role: u64 = 1u64 << 13;

        let (corp_id, medal_id) = {
            let (ctx, _clock) = open_ctx(&db_path, t0);
            ctx.db.with_mut_snapshot(|snap| {
                snap.skills.entry(PILOT_1).or_default().insert(
                    3363,
                    TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
                );
                snap.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_isk(5_000_000_000);
            });
            let mut sess1 = login_select(&ctx, 9101, "demo", "demo-secret", PILOT_1).await;
            let add_corp = rpc(
                &ctx,
                &mut sess1,
                "corpRegistry",
                "AddCorporation",
                &[PyRep::String("B29 Durable Corp".into()), PyRep::String("B29D".into()), PyRep::Real(0.05)],
            )
            .await;
            let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
            eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

            let mut sess2 = login_select(&ctx, 9102, "other", "other-secret", PILOT_2).await;
            let app_res = rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await;
            let app_id = app_res
                .dict_get("applicationID")
                .and_then(|v| v.as_i64())
                .or_else(|| app_res.as_i64())
                .expect("applicationID");
            rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;

            rpc(
                &ctx,
                &mut sess1,
                "corpRegistry",
                "UpdateTitle",
                &[PyRep::Integer(1), PyRep::String("Quartermaster".into()), PyRep::Integer(hangar_role as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)],
            )
            .await;
            rpc(
                &ctx,
                &mut sess1,
                "corpRegistry",
                "UpdateMember",
                &[
                    PyRep::Integer(PILOT_2),
                    PyRep::Integer((hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64),
                    PyRep::Integer(1),
                ],
            )
            .await;
            let medal_res = rpc(&ctx, &mut sess1, "corporationSvc", "CreateMedal", &[PyRep::String("B29 Star".into()), PyRep::String("Awarded".into()), PyRep::List(vec![])]).await;
            let medal_id = medal_res
                .as_i64()
                .or_else(|| medal_res.dict_get("medalID").and_then(|v| v.as_i64()))
                .expect("medal_id");
            rpc(&ctx, &mut sess1, "corporationSvc", "GiveMedalToCharacters", &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::String("Service".into())]).await;

            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
                snap.corporations.get_mut(&corp_id).unwrap().ticker = "ABORT".into();
                Err("abort".to_string())
            });
            assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].ticker.clone()), "B29D");
            (corp_id, medal_id)
        };

        {
            let (ctx2, _clock2) = open_ctx(&db_path, t0 + 60);
            let mut sess1 = login_select(&ctx2, 9103, "demo", "demo-secret", PILOT_1).await;
            let mut sess2 = login_select(&ctx2, 9104, "other", "other-secret", PILOT_2).await;
            let emp = rpc(&ctx2, &mut sess1, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(PILOT_2)]).await;
            assert!(pyrep_contains_int(&emp, corp_id));
            let audit = rpc(&ctx2, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)]).await;
            assert!(
                pyrep_contains_int(&audit, PILOT_2)
                    && pyrep_contains_int(&audit, (hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64)
            );
            let medals = rpc(&ctx2, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(PILOT_2)]).await;
            assert!(pyrep_contains_int(&medals, medal_id) && pyrep_contains_int(&medals, PILOT_2));
            ctx2.db.with_snapshot(|snap| {
                let gov = snap.corp_governance.get(&corp_id).expect("corp_governance");
                assert_eq!(gov.titles.get(&1).map(|t| t.0.as_str()), Some("Quartermaster"));
                assert!(gov.pending_role_grants.contains_key(&PILOT_2));
                assert!(gov.medals.iter().any(|m| m.0 == medal_id));
                assert!(gov.medal_recipients.iter().any(|r| r.0 == medal_id && r.1 == PILOT_2));
                assert!(snap.corporations[&corp_id].members.contains(&PILOT_2));
            });
        }
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b29_corp_invite_join_kick_quit_lifecycle() {
        // parity: feat:lifecycle.corp-invite-consent
        // parity: feat:lifecycle.corp-join-leaves-old-corp
        // parity: feat:lifecycle.corp-kick-online-session
        // parity: feat:lifecycle.corp-quit-cleanup
        eve_testkit::attest(&[
            "feat:lifecycle.corp-invite-consent",
            "feat:lifecycle.corp-join-leaves-old-corp",
            "feat:lifecycle.corp-kick-online-session",
            "feat:lifecycle.corp-quit-cleanup",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert(
                "third_b29".to_string(),
                eve_db::AccountRecord {
                    user_id: 30,
                    username: "third_b29".to_string(),
                    password_token: WorldRepository::hash_password_token("third_b29", "third-secret"),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
            ch3.character_id = PILOT_3;
            ch3.user_id = 30;
            ch3.name = "Third B29".into();
            ch3.station_id = Some(60_003_760);
            snap.characters.insert(PILOT_3, ch3);

            if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
                ch2.corporation_id = 98_000_001;
            }
            if let Some(old_corp) = snap.corporations.get_mut(&98_000_001) {
                old_corp.members.insert(PILOT_2);
                old_corp.member_roles.insert(PILOT_2, 0);
            }
        });

        let mut sess1 = login_select(&ctx, 9201, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = login_select(&ctx, 9202, "other", "other-secret", PILOT_2).await;

        let add_corp = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("B29 Lifecycle Corp".into()), PyRep::String("B29L".into()), PyRep::Real(0.05)],
        )
        .await;
        let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

        let orig_corp_p3 = ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id);
        assert!(rpc_res(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_3), PyRep::Bool(true)]).await.is_err());
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id), orig_corp_p3);

        let inv = rpc(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2), PyRep::Bool(false)]).await;
        let inv_id = inv.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
        let old_corp_p2 = sess2.corporation_id.unwrap();
        rpc(&ctx, &mut sess2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())]).await;
        assert_eq!(sess2.corporation_id, Some(corp_id));
        ctx.db.with_snapshot(|s| {
            assert!(s.corporations[&corp_id].members.contains(&PILOT_2));
            assert!(!s.corporations[&old_corp_p2].members.contains(&PILOT_2));
            assert!(!s.corporations[&old_corp_p2].member_roles.contains_key(&PILOT_2));
        });

        ctx.drain_notifications_for_char(PILOT_2);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
        rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await;
        let notifs = ctx.drain_notifications_for_char(PILOT_2);
        assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
        ctx.db.with_snapshot(|s| {
            assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
            if let Some(ch) = s.chat_channels.get(&format!("corpid:{corp_id}")) {
                assert!(!ch.members.contains(&PILOT_2));
            }
        });

        // Re-join PILOT_2, assign title + pending role grant + account key, then self-quit via KickOutMember(PILOT_2).
        ctx.advance_secs(25);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
        let inv2 = rpc(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2), PyRep::Bool(false)]).await;
        let inv2_id = inv2.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut sess2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())]).await;
        rpc(&ctx, &mut sess1, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Officer".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await;
        rpc(&ctx, &mut sess1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer((1u64 << 13) as i64), PyRep::Integer(1), PyRep::Integer(1001)]).await;

        ctx.advance_secs(25);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
        rpc(&ctx, &mut sess2, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await;
        let bloodline_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].bloodline_id);
        let expected_npc_corp = ctx.sde.bloodline(bloodline_id).expect("bloodline").corporation_id;
        assert_eq!(sess2.corporation_id, Some(expected_npc_corp));
        ctx.db.with_snapshot(|s| {
            if let Some(gov) = s.corp_governance.get(&corp_id) {
                assert!(!gov.member_titles.contains_key(&PILOT_2));
                assert!(!gov.pending_role_grants.contains_key(&PILOT_2));
                assert!(!gov.member_account_keys.contains_key(&PILOT_2));
            }
            assert!(!s.corp_member_titles.contains_key(&(corp_id, PILOT_2)));
            assert!(!s.chat_channels[&format!("corpid:{corp_id}")].members.contains(&PILOT_2));
            assert!(s.chat_channels[&format!("corpid:{expected_npc_corp}")].members.contains(&PILOT_2));
        });
    }

    #[tokio::test]
    async fn test_b29_crimewatch_persist_restart_and_movement_restrictions() {
        // parity: feat:lifecycle.crimewatch-state-persist
        // parity: feat:lifecycle.crimewatch-across-restart
        // parity: feat:lifecycle.crimewatch-highsec-criminal-movement
        eve_testkit::attest(&[
            "feat:lifecycle.crimewatch-state-persist",
            "feat:lifecycle.crimewatch-across-restart",
            "feat:lifecycle.crimewatch-highsec-criminal-movement",
        ]);

        let db_path = temp_sqlite_path("cw_persist");
        let t0 = 1_775_100_000u64;
        let jita = 30_000_142u32;

        let (crim_exp, susp_exp, zz_gate_in, zz_allowed_gate) = {
            let (ctx, _clock) = open_ctx(&db_path, t0);
            let pos = DVec3::new(100_000.0, 0.0, 0.0);
            let p1_skills = ctx.db.with_snapshot(|s| s.skills.get(&PILOT_1).cloned());
            let mut s1 = setup_pilot_in_space(&ctx, 9301, PILOT_1, 1_000_009, 86_300_001, 603, jita, pos).await;
            let _s2 = setup_pilot_in_space(&ctx, 9302, PILOT_2, 1_000_009, 86_300_002, 603, jita, pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
            let mut s3 = setup_pilot_in_space(&ctx, 9303, PILOT_3, 1_000_009, 86_300_003, 603, jita, pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(sk) = p1_skills {
                    snap.skills.insert(PILOT_3, sk);
                }
                snap.items.insert(86_300_010, item_rec(86_300_010, 448, PILOT_1, 86_300_001, InventoryFlag::MedSlot0, 1, true));
                snap.items.insert(86_300_012, item_rec(86_300_012, 1964, PILOT_3, 86_300_003, InventoryFlag::MedSlot0, 1, true));
            });
            let jita_h = ctx.systems.get_or_spawn(jita);
            for _ in 0..35 {
                let _ = jita_h.step_manual(1.0).await;
            }

            // 1. Live stargate jump into Zarzakh -> zarzakh_gate_locks
            let (&zz_gate_in, zz_jump) = ctx.sde.jumps.iter().find(|(_, j)| j.to_solar_system_id == 30_100_000).expect("gate to Zarzakh");
            let mut s4 = setup_pilot_in_space(&ctx, 9304, PILOT_4, 1_000_009, 86_300_004, 603, zz_jump.from_solar_system_id, zz_jump.position).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.characters.get_mut(&PILOT_4).unwrap().balance = IskCents::from_isk(50_000_000);
            });
            rpc(&ctx, &mut s4, "beyonce", "CmdStargateJump", &[PyRep::Integer(zz_gate_in as i64)]).await;

            // 2. Live illegal highsec warp-scramble -> criminal flag
            rpc(&ctx, &mut s1, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
            rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(86_300_002)]).await;
            ctx.advance_secs(10);
            for _ in 0..10 {
                let _ = jita_h.step_manual(1.0).await;
            }
            rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(86_300_010), PyRep::String("warpScramble".into()), PyRep::Integer(86_300_002)]).await;
            eve_server::services::combat_crimewatch::create_kill_right(&ctx, 500_001, PILOT_2, PILOT_1, 1_000_000.0);
            ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().security_status = -2.5);

            // 3. Live remote-assisting Criminal PILOT_1 in highsec -> suspect flag on PILOT_3
            rpc(&ctx, &mut s3, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
            rpc(&ctx, &mut s3, "dogmaIM", "AddTarget", &[PyRep::Integer(86_300_001)]).await;
            ctx.advance_secs(10);
            for _ in 0..10 {
                let _ = jita_h.step_manual(1.0).await;
            }
            rpc(&ctx, &mut s3, "dogmaIM", "Activate", &[PyRep::Integer(86_300_012), PyRep::None, PyRep::Integer(86_300_001)]).await;

            let (ce, se) = {
                let cw = ctx.crimewatch.read().unwrap();
                (
                    cw.states.get(&PILOT_1).map(|c| c.criminal_flag_until_unix).unwrap_or(0),
                    cw.states.get(&PILOT_3).map(|c| c.suspect_flag_until_unix).unwrap_or(0),
                )
            };
            assert!(ce > t0 && se > t0, "expected ce > t0 ({ce} vs {t0}) and se > t0 ({se} vs {t0})");
            (ce, se, zz_gate_in, zz_jump.destination_stargate_id)
        };

        {
            let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
            assert!(ctx2.db.with_snapshot(|s| s.characters[&PILOT_1].security_status < 0.0));
            let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
            let mut s1 = setup_pilot_in_space(&ctx2, 9305, PILOT_1, 1_000_009, 86_300_001, 603, jita, DVec3::new(100_000.0, 0.0, 0.0)).await;
            let st1 = rpc(&ctx2, &mut s1, "crimewatch", "GetClientStates", &[]).await;
            let PyRep::Tuple(top) = &st1 else { panic!("expected Tuple from GetClientStates, got {st1:?}"); };
            assert_eq!(top[3], PyRep::Integer(0));
            let PyRep::Tuple(timers) = &top[0] else { panic!("expected timers Tuple, got {:?}", top[0]); };
            assert_eq!(timers.get(3), Some(&PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(crim_exp))])));

            let mut s3 = SessionState::new(9306);
            s3.user_id = Some(30);
            s3.character_id = Some(PILOT_3);
            let st3 = rpc(&ctx2, &mut s3, "crimewatch", "GetClientStates", &[]).await;
            let PyRep::Tuple(top3) = &st3 else { panic!("expected Tuple from GetClientStates, got {st3:?}"); };
            let PyRep::Tuple(timers3) = &top3[0] else { panic!("expected timers Tuple, got {:?}", top3[0]); };
            assert!(timers3.contains(&PyRep::Tuple(vec![PyRep::Integer(302), PyRep::Integer(unix_to_ft(susp_exp))])));
            ctx2.db.with_snapshot(|snap| {
                assert!(!snap.kill_rights.is_empty());
                assert_eq!(snap.zarzakh_gate_locks.get(&PILOT_4).map(|(g, _)| *g), Some(zz_allowed_gate));
            });

            // Post-restart Zarzakh exit via a different gate must be rejected
            let (&other_zz_gate, other_zz_jump) = ctx2
                .sde
                .jumps
                .iter()
                .find(|(&gid, j)| j.from_solar_system_id == 30_100_000 && gid != zz_allowed_gate && gid != zz_gate_in)
                .expect("other Zarzakh gate");
            let mut s4 = setup_pilot_in_space(&ctx2, 9307, PILOT_4, 1_000_009, 86_300_004, 603, 30_100_000, other_zz_jump.position).await;
            let zz_err = rpc_res(&ctx2, &mut s4, "beyonce", "CmdStargateJump", &[PyRep::Integer(other_zz_gate as i64)]).await;
            assert!(zz_err.is_err());

            // All 4 live movement RPCs in highsec must reject criminal PILOT_1
            let cel_id = far_celestial_id(&ctx2, jita);
            let (&jita_gate, _) = ctx2.sde.jumps.iter().find(|(_, j)| j.from_solar_system_id == jita).expect("Jita gate");
            assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdWarpToStuff", &[PyRep::Integer(cel_id), PyRep::Real(0.0)]).await.is_err());
            assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdStargateJump", &[PyRep::Integer(jita_gate as i64)]).await.is_err());
            assert!(rpc_res(&ctx2, &mut s1, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_000_001)]).await.is_err());
            assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdJumpThroughFleet", &[PyRep::Integer(PILOT_2), PyRep::Integer(86_300_002), PyRep::Integer(jita as i64)]).await.is_err());
        }
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b29_drones_and_fighters_space_lifecycle() {
        // parity: feat:lifecycle.drones-leave-grid-abandon
        // parity: feat:lifecycle.drones-in-space-persist-restart
        // parity: feat:lifecycle.drones-recall-same-grid
        // parity: feat:lifecycle.fighters-left-in-space
        eve_testkit::attest(&[
            "feat:lifecycle.drones-leave-grid-abandon",
            "feat:lifecycle.drones-in-space-persist-restart",
            "feat:lifecycle.drones-recall-same-grid",
            "feat:lifecycle.fighters-left-in-space",
        ]);

        let db_path = temp_sqlite_path("drones_fighters");
        let t0 = 1_775_200_000u64;
        let sys_id = 30_000_142u32;
        let kw = HashMap::new();
        let (char_id, vexor_id, carrier_id) = (96_200_001i64, 86_200_099i64, 86_200_100i64);
        let (drone_1, drone_2, fighter_1) = (86_200_110i64, 86_200_111i64, 86_200_120i64);
        let (f_pilot, f_sess_id) = (96_200_003i64, 9403u64);
        let (station_pos, drone_ball_1) = {
            let (ctx, _clock) = open_ctx(&db_path, t0);
            let pos = ctx.sde.stations[&60_003_760].position;
            let mut sess = setup_pilot_in_space(&ctx, 9401, char_id, 1_000_125, vexor_id, 626, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.skills.entry(char_id).or_default().insert(
                    3436,
                    TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 },
                );
                for did in [drone_1, drone_2] {
                    snap.items.insert(did, item_rec(did, 2454, char_id, vexor_id, InventoryFlag::DroneBay, 1, false));
                }
                snap.items.insert(fighter_1, item_rec(fighter_1, 23055, f_pilot, carrier_id, InventoryFlag::FighterBay, 9, false));
            });

            let launch_rep = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_1), PyRep::Integer(drone_2)])], &kw, None).await.expect("LaunchDrones");
            let drone_ball_1 = match &launch_rep {
                PyRep::List(items) => items.first().and_then(|v| v.as_i64()).unwrap_or(drone_1),
                _ => drone_1,
            };

            let mut f_sess = setup_pilot_in_space(&ctx, f_sess_id, f_pilot, 1_000_125, carrier_id, 23757, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_1)], &kw, None).await.expect("LoadFightersToTube");
            ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("LaunchFightersFromTubes");
            assert_eq!(ctx.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);

            let sys_h = ctx.systems.get_or_spawn(sys_id);
            let (_, mut cball) = sys_h.get_ball_snapshot(vexor_id).await.unwrap().unwrap();
            cball.position = pos + DVec3::new(1_000.0, 0.0, 0.0);
            cball.velocity = DVec3::ZERO;
            cball.speed_fraction = 0.0;
            cball.mode = MovementMode::Stop;
            let _ = sys_h.spawn_ball(cball).await;
            ctx.advance_secs(12);
            ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Dock");
            ctx.dispatch_rpc(&mut f_sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Carrier Dock");
            assert_eq!(ctx.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);
            assert!(ctx.active_fighters.read().unwrap().values().all(|f| f.is_abandoned));

            let _ = ctx.dispatch_rpc(&mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(drone_ball_1)])], &kw, None).await;
            assert_eq!(ctx.db.get_item(drone_1).unwrap().location_id, sys_id as i64);
            (pos, drone_ball_1)
        };

        {
            let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
            let (scooper_id, scooper_ship) = (96_200_002i64, 86_200_200i64);
            let mut scooper_sess = setup_pilot_in_space(&ctx2, 9402, scooper_id, 1_000_009, scooper_ship, 626, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
            assert!(sys_h2.get_ball_snapshot(drone_ball_1).await.unwrap().is_some());
            ctx2.dispatch_rpc(&mut scooper_sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(drone_ball_1)], &kw, None).await.expect("ScoopToDroneBay");
            assert_eq!(ctx2.db.get_item(drone_1).unwrap().location_id, scooper_ship);

            // Undock carrier, scoop abandoned fighter, load into tube 0, and re-launch
            let mut f_sess2 = setup_pilot_in_space(&ctx2, f_sess_id, f_pilot, 1_000_125, carrier_id, 23757, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_1)], &kw, None).await.expect("ScoopFighter");
            assert_eq!(ctx2.db.get_item(fighter_1).unwrap().location_id, carrier_id);
            ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_1)], &kw, None).await.expect("ReloadTube");
            ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("RelaunchFighter");
            assert_eq!(ctx2.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);
        }
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b29_fleet_grace_invite_and_death_courier_lifecycle() {
        // parity: feat:lifecycle.fleet-disconnect-grace
        // parity: feat:lifecycle.fleet-invite-offline
        // parity: feat:lifecycle.death-session-change
        // parity: feat:lifecycle.courier-package-destroyed
        eve_testkit::attest(&[
            "feat:lifecycle.fleet-disconnect-grace",
            "feat:lifecycle.fleet-invite-offline",
            "feat:lifecycle.death-session-change",
            "feat:lifecycle.courier-package-destroyed",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let (cargo_1, cargo_2) = (620_003i64, 620_004i64);
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
            ch3.character_id = PILOT_3;
            ch3.name = "Offline Fleet Target".into();
            snap.characters.insert(PILOT_3, ch3);
            snap.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_isk(500_000_000);
            snap.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_isk(500_000_000);
            snap.items.insert(cargo_1, item_rec(cargo_1, 34, PILOT_2, 60_003_760, InventoryFlag::Hangar, 100, false));
            snap.items.insert(cargo_2, item_rec(cargo_2, 34, PILOT_2, 60_003_760, InventoryFlag::Hangar, 50, false));
        });
        let mut s1 = login_select(&ctx, 9501, "demo", "demo-secret", PILOT_1).await;
        let mut s2 = login_select(&ctx, 9502, "other", "other-secret", PILOT_2).await;

        let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        assert!(rpc_res(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_3), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.is_err());

        rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
        ctx.advance_secs(FLEET_INVITE_EXPIRY_SECS + 5);
        tick_fleet_lifecycle(&ctx);
        assert!(rpc_res(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await.is_err());

        rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
        rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await;
        handle_fleet_member_disconnect(&ctx, &mut s2);
        ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid].members.contains(&PILOT_2)));
        ctx.advance_secs(FLEET_DISCONNECT_GRACE_SECS + 5);
        tick_fleet_lifecycle(&ctx);
        ctx.db.with_snapshot(|snap| assert!(!snap.fleets[&fid].members.contains(&PILOT_2)));

        // Live CreateContract & AcceptContract for Courier #1 (1,000,000 ISK collateral)
        let mut s2 = login_select(&ctx, 9503, "other", "other-secret", PILOT_2).await;
        let c1_rep = rpc(&ctx, &mut s2, "contractProxy", "CreateContract", &[
            PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(60), PyRep::Integer(1),
            PyRep::Integer(60_003_760), PyRep::Integer(60_004_588), PyRep::Real(0.0), PyRep::Real(50_000.0),
            PyRep::Real(1_000_000.0), PyRep::String("Courier 1".into()), PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cargo_1), PyRep::Integer(100)])]),
        ]).await;
        let c1_id = c1_rep.as_i64().or_else(|| c1_rep.dict_get("contractID").and_then(|v| v.as_i64())).expect("c1_id");
        rpc(&ctx, &mut s1, "contractProxy", "AcceptContract", &[PyRep::Integer(c1_id)]).await;
        ctx.db.with_mut_snapshot(|snap| {
            let ship_id = snap.characters[&PILOT_1].active_ship_id;
            if let Some(wrap) = snap.items.values_mut().find(|it| it.type_id == 3468 && it.location_id == 60_003_760) {
                wrap.location_id = ship_id;
                wrap.flag = InventoryFlag::Cargo;
            }
        });

        // Undock PILOT_1 into space, then self-destruct ship via live ship.SelfDestruct
        let ship1_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
        s1 = setup_pilot_in_space(&ctx, 1, PILOT_1, 1_000_009, ship1_id, 603, 30_000_142, DVec3::new(100_000.0, 0.0, 0.0)).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
        let p2_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].balance);
        ctx.drain_notifications_for_char(PILOT_1);

        rpc_res(&ctx, &mut s1, "ship", "SelfDestruct", &[]).await.expect("SD1");
        ctx.advance_secs(125);
        rpc_res(&ctx, &mut s1, "ship", "SelfDestruct", &[]).await.expect("SD2");

        let capsule_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
        let ship_death_notifs = ctx.drain_notifications_for_char(PILOT_1);
        assert!(session_changed_has_field(&ship_death_notifs, "shipid", capsule_id));
        assert!(rpc_res(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)]).await.is_err());

        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.contracts[&c1_id].status, "Failed");
            assert_eq!(snap.characters[&PILOT_2].balance.0, p2_bal_before.0 + IskCents::from_isk(1_050_000).0);
            assert!(!snap.items.values().any(|it| it.type_id == 3468 && it.location_id == 60_003_760));
        });

        // Pod self-destruct -> medical-clone station respawn + SessionChangeInProgress on immediate Undock
        ctx.advance_secs(15);
        let mut s4_pod = setup_pilot_in_space(&ctx, 9505, PILOT_4, 1_000_009, 620_099, 670, 30_000_142, DVec3::new(100_000.0, 0.0, 0.0)).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_4);
        ctx.drain_notifications_for_char(PILOT_4);
        rpc(&ctx, &mut s4_pod, "ship", "SelfDestruct", &[]).await;
        ctx.advance_secs(125);
        if ctx.db.with_snapshot(|s| s.characters[&PILOT_4].station_id.is_none()) {
            rpc(&ctx, &mut s4_pod, "ship", "SelfDestruct", &[]).await;
        } else {
            s4_pod.station_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_4].station_id);
        }
        let pod_notifs = ctx.drain_notifications_for_char(PILOT_4);
        assert!(session_changed_has_field(&pod_notifs, "stationid", 60_003_760));
        assert_eq!(s4_pod.station_id, Some(60_003_760));
        let undock_err = rpc_res(&ctx, &mut s4_pod, "stationSvc", "Undock", &[PyRep::Integer(60_003_760)])
            .await
            .expect_err("immediate Undock after pod respawn must be rejected");
        assert_eq!(undock_err.code, "SessionChangeInProgress");

        // Courier #2 deadline failure while in space -> Failed with collateral, wrap NOT teleported to start station
        ctx.advance_secs(15);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
        let c2_rep = rpc(&ctx, &mut s2, "contractProxy", "CreateContract", &[
            PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(60), PyRep::Integer(1),
            PyRep::Integer(60_003_760), PyRep::Integer(60_004_588), PyRep::Real(0.0), PyRep::Real(50_000.0),
            PyRep::Real(500_000.0), PyRep::String("Courier 2".into()), PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cargo_2), PyRep::Integer(50)])]),
        ]).await;
        let c2_id = c2_rep.as_i64().or_else(|| c2_rep.dict_get("contractID").and_then(|v| v.as_i64())).expect("c2_id");
        let mut s1_c2 = login_select(&ctx, 9506, "demo", "demo-secret", PILOT_1).await;
        rpc(&ctx, &mut s1_c2, "contractProxy", "AcceptContract", &[PyRep::Integer(c2_id)]).await;
        let ship2_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(wrap) = snap.items.values_mut().find(|it| it.type_id == 3468 && it.location_id == 60_003_760) {
                wrap.location_id = ship2_id;
                wrap.flag = InventoryFlag::Cargo;
            }
        });
        rpc(&ctx, &mut s1_c2, "stationSvc", "Undock", &[PyRep::Integer(60_003_760)]).await;
        ctx.advance_secs(90_000);
        rpc(&ctx, &mut s1_c2, "contractProxy", "FailContract", &[PyRep::Integer(c2_id)]).await;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.contracts[&c2_id].status, "Failed");
            assert_eq!(snap.items[&cargo_2].location_id, ship2_id);
            assert_eq!(snap.items[&cargo_2].owner_id, PILOT_1);
        });
    }
}

pub(crate) mod fleet_lifecycle_kick_login_logout {
    // End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 1).
    //
    // Covers:
    // - `feat:lifecycle.fleet-kick-session`, `feat:lifecycle.fleet-login-consistency`,
    //   `feat:lifecycle.fleet-logout`, `feat:lifecycle.fleet-server-restart`
    // - `feat:lifecycle.industry-pause-and-many`, `feat:lifecycle.owner_notes-persistence`
    // - `feat:lifecycle.industry-pi-durable`, `feat:lifecycle.market-wallet-history-durable`


    use crate::common;
    use eve_db::{
        corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
        TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::{
        load_job_cost_meta, save_orbital_settings, OrbitalCustomsSettings,
    };
    use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
    use eve_server::services::wallet_serverops::create_bill;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const CORP_ID: i64 = 2001;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const FH: &str = "fleetObjectHandler";

    fn temp_db_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_b30_{tag}_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_fresh_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
        repo.with_mut_snapshot(|s| *s = demo);
        let ctx = ServerContext::with_repository(Arc::new(repo));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx
    }

    fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
        let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
        let ctx = ServerContext::with_repository(Arc::new(repo));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
    }

    async fn rpc_kw(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
        kw: &[(&str, PyRep)],
    ) -> Result<PyRep, BridgeRpcError> {
        let kwargs: HashMap<String, PyRep> = kw.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        ctx.dispatch_rpc(sess, svc, None, method, args, &kwargs, None).await
    }

    async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
        ctx.advance_secs(12);
        sess
    }

    fn rowset_maps(rep: &PyRep) -> Vec<HashMap<String, PyRep>> {
        let PyRep::Rowset { columns, rows } = rep else { panic!("expected Rowset, got {rep:?}") };
        rows.iter().map(|r| columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
    }

    fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
    }

    fn seed_corp_full(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, corp_id: i64) {
        let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;
        let mut corp = CorporationRecord {
            corporation_id: corp_id, name: format!("B30 Corp {corp_id}"), ticker: "B30".into(), ceo_id: char_id,
            hq_station_id: Some(JITA_STATION_ID), members: [char_id].into_iter().collect(),
            member_roles: HashMap::from([(char_id, roles)]), ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(50_000_000);
        }
        corp.master_wallet = IskCents::from_isk(50_000_000);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(100_000_000);
        }
    }

    fn pi_cmd(op: i64, a: i64, b: i64, c: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(op), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(c)])
    }

    #[tokio::test]
    async fn test_b30_fleet_lifecycle_kick_login_logout_and_restart() {
        // parity: feat:lifecycle.fleet-kick-session, feat:lifecycle.fleet-login-consistency, feat:lifecycle.fleet-logout, feat:lifecycle.fleet-server-restart
        eve_testkit::attest(&[
            "feat:lifecycle.fleet-kick-session",
            "feat:lifecycle.fleet-login-consistency",
            "feat:lifecycle.fleet-logout",
            "feat:lifecycle.fleet-server-restart",
        ]);

        let db_path = temp_db_path("fleet");
        let ctx = open_fresh_ctx_at(&db_path, 1_760_000_000);
        let uid3 = ctx.db.provision_player_account("b30_fl3", "b30_fl3_pass");
        let p3 = ctx.db.create_character(uid3, "B30 Fleet 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

        let mut s1 = login_select(&ctx, 9101, "demo", "demo-secret", P1).await;
        let mut s2 = login_select(&ctx, 9102, "other", "other-secret", P2).await;
        let mut s3 = login_select(&ctx, 9103, "b30_fl3", "b30_fl3_pass", p3).await;

        let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        let wid = rpc(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Wing 1".into())]).await.unwrap().dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
        let sid = rpc(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid), PyRep::String("Squad 1".into())]).await.unwrap().dict_get("squadID").and_then(|v| v.as_i64()).unwrap();

        // `fleet-login-consistency`: CreateFleet, AcceptInvite, ApplyToJoinFleet refuse character already in a fleet; GetFleetID agrees
        let fid_b = rpc(&ctx, &mut s3, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s3, FH, "Invite", &[PyRep::Integer(P2)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(rpc(&ctx, &mut s1, FH, "AcceptInvite", &[PyRep::Integer(fid_b)]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(rpc(&ctx, &mut s1, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fid_b)]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(rpc(&ctx, &mut s1, FH, "GetFleetID", &[]).await, Ok(PyRep::Integer(fid)));
        assert_eq!(s1.fleet_id, Some(fid));
        ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid].members.contains(&P1)));
        rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();

        // `fleet-kick-session`: KickMember pushes OnSessionChanged clearing fleetid and blocks kicked member's RPCs
        rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::None, PyRep::Integer(2)]).await.unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
        rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
        assert_eq!(s2.fleet_id, Some(fid));
        ctx.drain_notifications_for_char(P2);
        rpc(&ctx, &mut s1, FH, "KickMember", &[PyRep::Integer(P2)]).await.unwrap();
        ctx.db.with_snapshot(|snap| assert_eq!(snap.characters[&P2].fleet_id, None));
        assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| {
            n.event == "OnSessionChanged"
                && n.args.iter().any(|a| a.dict_get("fleetid") == Some(&PyRep::Tuple(vec![PyRep::Integer(fid), PyRep::None])))
        }));
        assert_eq!(rpc(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3)]).await.unwrap_err().code, "NotInFleet");
        assert_eq!(rpc(&ctx, &mut s2, FH, "SendBroadcast", &[PyRep::String("Target".into()), PyRep::Integer(0)]).await.unwrap_err().code, "NotInFleet");

        // `fleet-logout`: Logout removes from slots/channel, emits OnFleetLeave, passes boss, and disbands empty fleet
        rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();
        let mut s3 = login_select(&ctx, 9105, "b30_fl3", "b30_fl3_pass", p3).await;
        rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::None, PyRep::Integer(2)]).await.unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
        rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
        rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3), PyRep::Integer(wid), PyRep::Integer(sid), PyRep::Integer(3)]).await.unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, p3);
        rpc(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
        for c in [P1, P2, p3] {
            ctx.drain_notifications_for_char(c);
        }

        rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();
        ctx.db.with_snapshot(|snap| {
            let f = &snap.fleets[&fid];
            assert!(!f.members.contains(&p3) && !f.member_details.contains_key(&p3));
            assert_eq!(f.squad_details[&sid].commander_id, None);
            assert!(!snap.chat_channels[&format!("fleetid:{fid}")].members.contains(&p3));
        });
        assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(p3)]));

        rpc(&ctx, &mut s1, "authentication", "Logout", &[]).await.unwrap();
        ctx.db.with_snapshot(|snap| {
            let f = &snap.fleets[&fid];
            assert_eq!(f.boss_id, P2);
            assert_ne!(f.member_details[&P2].job & 2, 0);
        });
        let p2_notifs = ctx.drain_notifications_for_char(P2);
        assert!(p2_notifs.iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(P1)]));
        assert!(p2_notifs.iter().any(|n| n.event == "OnFleetMemberChanged" && n.args.get(7).and_then(|v| v.as_i64()).is_some_and(|j| (j & 2) != 0)));

        rpc(&ctx, &mut s2, "authentication", "Logout", &[]).await.unwrap();
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.fleets.contains_key(&fid) && !snap.chat_channels.contains_key(&format!("fleetid:{fid}")));
        });

        // `fleet-server-restart`: Server restart drops offline fleets so no ghost members survive
        let mut s2_new = login_select(&ctx, 9106, "other", "other-secret", P2).await;
        let fid2 = rpc(&ctx, &mut s2_new, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        drop(ctx);
        let ctx2 = open_ctx_at(&db_path, 1_760_001_000);
        let mut s2_relog = login_select(&ctx2, 9107, "other", "other-secret", P2).await;
        assert_eq!(s2_relog.fleet_id, None);
        assert_eq!(rpc(&ctx2, &mut s2_relog, FH, "GetFleetID", &[]).await.unwrap(), PyRep::None);
        ctx2.db.with_snapshot(|snap| assert!(!snap.fleets.contains_key(&fid2)));
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b30_industry_pause_many_and_owner_notes_persistence() {
        // parity: feat:lifecycle.industry-pause-and-many, feat:lifecycle.owner_notes-persistence
        eve_testkit::attest(&[
            "feat:lifecycle.industry-pause-and-many",
            "feat:lifecycle.owner_notes-persistence",
        ]);

        let db_path = temp_db_path("industry_owner");
        let base_u = 1_760_000_000u64;
        let ctx = open_fresh_ctx_at(&db_path, base_u);
        let struct_id = 1_000_000_251i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_corp_full(snap, P1, CORP_ID);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = None;
                ch.structure_id = Some(struct_id);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            for &(tid, lvl) in &[(3380, 5), (3387, 5), (2495, 5), (2505, 5), (3443, 5), (3447, 5), (16596, 5), (13279, 4), (2406, 3), (2403, 2)] {
                snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
            snap.structures.insert(struct_id, UpwellStructureRecord {
                structure_id: struct_id, type_id: 35826, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                name: "B30 Azbel".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 5_000, shield_hp: 10000.0, armor_hp: 10000.0,
                hull_hp: 10000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut svc = make_item(struct_id + 1, 35878, CORP_ID, struct_id, InventoryFlag::ServiceSlot0, 1, true);
            svc.dynamic_attributes.insert(2, 1.0);
            snap.items.insert(struct_id + 1, svc);
            snap.module_online_states.insert(struct_id + 1, true);
            for iid in [710_001, 710_002, 710_003, 710_004, 710_005] {
                snap.items.insert(iid, make_item(iid, 785, P1, struct_id, InventoryFlag::Hangar, 1, true));
            }
            for (iid, tid, qty) in [(710_010, 34u32, 5_000_000), (710_011, 35u32, 1_000_000), (710_012, 36u32, 500_000)] {
                snap.items.insert(iid, make_item(iid, tid, P1, struct_id, InventoryFlag::Hangar, qty, false));
            }
            snap.items.insert(710_020, make_item(710_020, 691, CORP_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
            snap.items.insert(710_021, make_item(710_021, 34, CORP_ID, JITA_STATION_ID, InventoryFlag::Hangar, 100_000, false));
        });

        let mut sess = login_select(&ctx, 9201, "demo", "demo-secret", P1).await;
        sync_all_structures(&ctx);
        ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 1);

        let install_mfg = |bp_id: i64, runs: i64| PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bp_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(runs)),
        ]);
        let j1 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_001, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let j2 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_002, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let j_long = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_003, 10)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        ctx.advance_secs(7_200);
        sync_all_structures(&ctx);
        let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.industry_jobs[&j1].status, "Active");
            assert_eq!(snap.industry_jobs[&j_long].status, "Paused");
            assert_eq!(load_job_cost_meta(snap, j_long).unwrap().paused_at_unix, Some(base_u + 12 + 3_600));
        });

        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 5_000;
            snap.items.get_mut(&(struct_id + 1)).unwrap().dynamic_attributes.insert(2, 1.0);
            snap.module_online_states.insert(struct_id + 1, true);
        });
        sync_all_structures(&ctx);
        let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
        let many_rep = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(j1), PyRep::Integer(j2)])]).await.unwrap();
        let many_str = format!("{many_rep:?}");
        assert!(many_str.contains(&j1.to_string()) && many_str.contains(&j2.to_string()));

        let j_corp = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(710_004)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
            (PyRep::String("ownerID".into()), PyRep::Integer(CORP_ID)),
        ])]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        // `owner_notes-persistence`: place order (duration 1), install corp job at station, build PI network via RPCs
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.structure_id = None;
                ch.station_id = Some(JITA_STATION_ID);
            }
        });
        sess.structure_id = None;
        sess.station_id = Some(JITA_STATION_ID);
        let buy_res = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(3.50),
            PyRep::Integer(1_000), PyRep::Integer(5), PyRep::Integer(10),
            PyRep::Integer(1), PyRep::Bool(true), PyRep::Integer(1002),
        ]).await.unwrap();
        let order_id = buy_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        let corp_orders = rowset_maps(&rpc(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
        let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        let issue_ft = ord_row.get("issueDate").and_then(|v| v.as_i64()).unwrap();
        let escrow_isk = ord_row.get("escrow").and_then(|v| v.as_f64()).unwrap();

        let j_corp_deliver = rpc_kw(
            &ctx, &mut sess, "industryManager", "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
            &[("useCorp", PyRep::Bool(true))],
        ).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        let ship_id = sess.ship_id.unwrap();
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = None;
            }
            snap.items.insert(710_030, make_item(710_030, 2534, P1, ship_id, InventoryFlag::Cargo, 1, false));
        });
        let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap();
        let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!() };
        let p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap();
        let pi_cmds = vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
            PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2848), PyRep::Real(0.11), PyRep::Real(0.11)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
            pi_cmd(3, 101, 102, 0),
            pi_cmd(3, 102, 103, 0),
            pi_cmd(5, 102, 103, 1),
            PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(121)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(p0_tid), PyRep::Integer(3_000)]),
            PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(p0_tid), PyRep::Real(0.02), PyRep::Integer(3)]),
        ];
        rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(pi_cmds)]).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = Some(JITA_STATION_ID);
            }
        });

        drop(ctx);
        let ctx2 = open_ctx_at(&db_path, base_u + 50_400);
        let mut sess2 = login_select(&ctx2, 9202, "demo", "demo-secret", P1).await;

        let corp_orders2 = rowset_maps(&rpc(&ctx2, &mut sess2, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
        let ord2 = corp_orders2.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        assert_eq!(ord2.get("issueDate").and_then(|v| v.as_i64()), Some(issue_ft));
        assert_eq!(ord2.get("duration").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(ord2.get("range").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(ord2.get("accountID").and_then(|v| v.as_i64()), Some(1002));

        let div3_before = ctx2.db.with_snapshot(|s| s.corporations[&CORP_ID].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance.0);
        ctx2.advance_secs(43_201);
        let hist_rows = rowset_maps(&rpc_kw(&ctx2, &mut sess2, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)], &[("isCorp", PyRep::Bool(true))]).await.unwrap());
        let exp_row = hist_rows.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
        assert_eq!(exp_row.get("orderState").and_then(|v| v.as_i64()), Some(4));
        let div3_after = ctx2.db.with_snapshot(|s| s.corporations[&CORP_ID].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance.0);
        assert_eq!(div3_after - div3_before, (escrow_isk * 100.0).round() as i64);

        let deliv = rpc(&ctx2, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(j_corp_deliver)]).await.unwrap();
        let deliv_id = deliv.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
        ctx2.db.with_snapshot(|s| {
            assert_eq!(s.items[&deliv_id].type_id, 587);
            assert_eq!(s.items[&deliv_id].owner_id, CORP_ID);
        });

        let net = rpc(&ctx2, &mut sess2, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)]).await.unwrap();
        let PyRep::List(pins) = net.dict_get("pins").unwrap() else { panic!() };
        let PyRep::List(links) = net.dict_get("links").unwrap() else { panic!() };
        let PyRep::List(routes) = net.dict_get("routes").unwrap() else { panic!() };
        assert_eq!((pins.len(), links.len(), routes.len()), (3, 2, 1));
        let ext_id = pins.iter().find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(2848)).unwrap().dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let ext_pin = rpc(&ctx2, &mut sess2, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)]).await.unwrap();
        assert_eq!(ext_pin.dict_get("headCount").and_then(|v| v.as_i64()), Some(3));

        // Ex-member cannot cancel or deliver corp job
        ctx2.db.with_mut_snapshot(|snap| snap.characters.get_mut(&P1).unwrap().corporation_id = 9999);
        assert_eq!(rpc(&ctx2, &mut sess2, "industryManager", "CancelJob", &[PyRep::Integer(j_corp)]).await.unwrap_err().code, "JobOwnershipViolation");
        assert_eq!(rpc(&ctx2, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(j_corp)]).await.unwrap_err().code, "JobOwnershipViolation");
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b30_industry_pi_and_market_wallet_history_durable() {
        // parity: feat:lifecycle.industry-pi-durable, feat:lifecycle.market-wallet-history-durable
        eve_testkit::attest(&[
            "feat:lifecycle.industry-pi-durable",
            "feat:lifecycle.market-wallet-history-durable",
        ]);

        let db_path = temp_db_path("pi_wallet");
        let base_u = 1_800_000_000u64;
        let ctx = open_fresh_ctx_at(&db_path, base_u);
        let athanor_id = 1_045_000_000_101i64;
        let metenox_id = 1_045_000_000_102i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_corp_full(snap, P1, CORP_ID);
            snap.characters.get_mut(&P1).unwrap().standings.insert(1_000_084, 6.0);
            snap.characters.get_mut(&P2).unwrap().balance = IskCents::from_isk(500_000_000);
            for &(tid, lvl) in &[(2403, 5), (2406, 5), (2495, 5), (2505, 5), (3380, 5), (3386, 5), (3402, 5), (11448, 4), (13279, 5), (46152, 5)] {
                snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
            for (iid, tid, qty, sing) in [(910_002, 691u32, 1, true), (910_003, 34u32, 5_000_000, false), (910_004, 35u32, 1_000_000, false), (910_008, 52_568u32, 500, false), (910_009, 587u32, 1, false)] {
                snap.items.insert(iid, make_item(iid, tid, P1, JITA_STATION_ID, InventoryFlag::Hangar, qty, sing));
            }
            snap.structures.insert(athanor_id, UpwellStructureRecord {
                structure_id: athanor_id, type_id: 35835, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                name: "B30 Athanor".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 500, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.structures.insert(metenox_id, UpwellStructureRecord {
                structure_id: metenox_id, type_id: 81826, owner_corp_id: CORP_ID, solar_system_id: 30_004_608,
                name: "B30 Metenox".into(), position: [1000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 200, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(910_020, make_item(910_020, 81143, CORP_ID, metenox_id, InventoryFlag::Hangar, 500, false));
            save_orbital_settings(snap, JITA_PLANET_ID, &OrbitalCustomsSettings { planet_id: JITA_PLANET_ID, owner_corp_id: CORP_ID, ..Default::default() });
        });

        let mut s1 = login_select(&ctx, 9601, "demo", "demo-secret", P1).await;
        let mut s2 = login_select(&ctx, 9602, "other", "other-secret", P2).await;

        rpc(&ctx, &mut s1, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)]).await.unwrap();
        ctx.advance_secs(511_200);
        ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200; s.items.get_mut(&910_020).unwrap().quantity = 500; s.auto_moon_miners.remove(&metenox_id); });
        ctx.advance_secs(7_200);
        rpc(&ctx, &mut s1, "autoMoonMining", "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)]).await.unwrap();
        rpc_kw(&ctx, &mut s1, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(JITA_PLANET_ID)], &[("taxRateValues", PyRep::keyval([("corporation", PyRep::Real(0.075))]))]).await.unwrap();

        // Build PI colony network via UserUpdateNetwork for durable PI verification
        let ship_pi = s1.ship_id.unwrap();
        s1.station_id = None;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = None; }
            s.items.insert(910_040, make_item(910_040, 2534, P1, ship_pi, InventoryFlag::Cargo, 1, false));
        });
        rpc(&ctx, &mut s1, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(201), PyRep::Integer(2534), PyRep::Real(0.20), PyRep::Real(0.20)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(202), PyRep::Integer(2848), PyRep::Real(0.22), PyRep::Real(0.22)]),
            pi_cmd(3, 201, 202, 0),
        ])]).await.unwrap();
        s1.station_id = Some(JITA_STATION_ID);
        ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); } });

        let rnd = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.station_id.is_some()).min_by_key(|a| a.agent_id).cloned().unwrap();
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&P1) { ch.standings.insert(rnd.agent_id, 9.0); ch.solar_system_id = rnd.solar_system_id.unwrap(); ch.station_id = rnd.station_id; }
            let sk = s.skills.entry(P1).or_default();
            for sid in [3402_u32, 3392_u32, 11448_u32] {
                sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
        });
        s1.solar_system_id = rnd.solar_system_id;
        s1.station_id = rnd.station_id;
        rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd.agent_id), PyRep::Integer(11448)]).await.unwrap();
        s1.solar_system_id = Some(JITA_SYSTEM_ID);
        s1.station_id = Some(JITA_STATION_ID);
        ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.solar_system_id = JITA_SYSTEM_ID; ch.station_id = Some(JITA_STATION_ID); } });
        let PyRep::Tuple(jp) = rpc(&ctx, &mut s1, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap() else { panic!() };
        let PyRep::List(rl) = &jp[1] else { panic!() };
        let rnd_rp_before = rl[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let job_id = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(2)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let sell_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(910_003)), (PyRep::String("typeID".into()), PyRep::Integer(34)),
            (PyRep::String("price".into()), PyRep::Real(4.50)), (PyRep::String("quantity".into()), PyRep::Integer(1_000)),
            (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]), PyRep::Bool(false)]).await.unwrap().dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s2, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(4.50), PyRep::Integer(400), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)]).await.unwrap();
        let char_orders_before = rowset_maps(&rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap());
        let sell_row_b = char_orders_before.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(sell_oid)).unwrap();
        let sell_issue_ft = sell_row_b.get("issueDate").and_then(|v| v.as_i64()).unwrap();
        let old_hist_before = rpc(&ctx, &mut s1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap();
        let char_tx_before = rpc(&ctx, &mut s1, "marketProxy", "CharGetTransactions", &[]).await.unwrap();
        let corp_tx_before = rpc(&ctx, &mut s1, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(1000)]).await.unwrap();
        let acct_tx_before = rpc(&ctx, &mut s1, "account", "GetTransactions", &[PyRep::Integer(1000)]).await.unwrap();
        rpc_kw(&ctx, &mut s1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(50_000.0), PyRep::Integer(1000)], &[("reason", PyRep::String("B30 dividend".into()))]).await.unwrap();
        let bill_id = create_bill(&ctx, 2, CORP_ID, 1_000_035, 250_000.0, ctx.now_unix() + 86_400, 0.0, JITA_STATION_ID, 0);
        rpc(&ctx, &mut s1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Integer(1000)]))])]).await.unwrap();
        let trade_id = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.unwrap().dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s1, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(125_000.0)]).await.unwrap();
        let raffle_id = rpc(&ctx, &mut s1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(910_009)), (PyRep::String("ticket_count".into()), PyRep::Integer(8)), (PyRep::String("ticket_price".into()), PyRep::Real(100_000.0)),
        ])]).await.unwrap().dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
        let frac = rpc(&ctx, &mut s1, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await.unwrap();
        let PyRep::List(ast_ids) = frac.dict_get("asteroidIDs").unwrap() else { panic!() };
        let first_ast_id = ast_ids[0].as_i64().unwrap();
        let undock = rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
        let m_ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
        ctx.db.with_mut_snapshot(|s| {
            if let Some(sh) = s.items.get_mut(&m_ship_id) { sh.type_id = 32880; }
            s.items.retain(|_, it| it.location_id != m_ship_id);
            s.items.insert(910_030, make_item(910_030, 483, P1, m_ship_id, InventoryFlag::HiSlot0, 1, true));
        });
        ctx.invalidate_ship_fitting(m_ship_id);
        let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        let undock_pos = jita.get_ball_snapshot(m_ship_id).await.unwrap().unwrap().1.position;
        let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
        if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(m_ship_id).await { sb.position = ast_pos + glam::DVec3::new(500.0, 0.0, 0.0); let _ = jita.spawn_ball(sb).await; }
        let _ = crate::mining_cycle_engine::mine_once(&ctx, &mut s1, 910_030, first_ast_id).await.unwrap();
        if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(m_ship_id).await { sb.position = undock_pos; let _ = jita.spawn_ball(sb).await; }
        rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();

        let saved_u = ctx.now_unix();
        let (wj_before, wt_before) = ctx.db.with_snapshot(|s| (s.wallet_journal.len(), s.char_wallet_transactions.len()));
        drop(ctx);

        let ctx2 = open_ctx_at(&db_path, saved_u + 3_600);
        let mut s1_re = login_select(&ctx2, 9603, "demo", "demo-secret", P1).await;
        assert_eq!(rpc(&ctx2, &mut s1_re, "planetOrbitalRegistryBroker", "GetTaxRate", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap().as_f64(), Some(0.075));
        let pi_net = rpc(&ctx2, &mut s1_re, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)]).await.unwrap();
        let PyRep::List(pi_pins) = pi_net.dict_get("pins").unwrap() else { panic!() };
        assert_eq!(pi_pins.len(), 2);
        assert_eq!(pi_net.dict_get("links").and_then(|v| match v { PyRep::List(l) => Some(l.len()), _ => None }), Some(1));
        let pin0_id = pi_pins[0].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        assert!(rpc(&ctx2, &mut s1_re, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(pin0_id)]).await.is_ok());

        let PyRep::Tuple(ext_tup) = rpc(&ctx2, &mut s1_re, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(athanor_id)]).await.unwrap() else { panic!() };
        assert!(ext_tup[0].dict_get("status").is_some());
        let met_det = rpc(&ctx2, &mut s1_re, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.unwrap();
        assert_eq!(met_det.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
        let obs = rpc(&ctx2, &mut s1_re, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.unwrap();
        assert!(format!("{obs:?}").contains(&P1.to_string()));
        let PyRep::Tuple(jp2) = rpc(&ctx2, &mut s1_re, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap() else { panic!() };
        let PyRep::List(rl2) = &jp2[1] else { panic!() };
        assert!(rl2[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0) > rnd_rp_before);
        let jobs_rep = rpc(&ctx2, &mut s1_re, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
        assert!(format!("{jobs_rep:?}").contains(&job_id.to_string()));
        let PyRep::List(job_list) = &jobs_rep else { panic!() };
        let j_row = job_list.iter().find(|r| r.dict_get("jobID").and_then(|v| v.as_i64()) == Some(job_id)).unwrap();
        assert!(j_row.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
        assert!(j_row.dict_get("totalJobCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

        assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap(), old_hist_before);
        let char_orders_after = rowset_maps(&rpc(&ctx2, &mut s1_re, "marketProxy", "GetCharOrders", &[]).await.unwrap());
        let sell_row_a = char_orders_after.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(sell_oid)).unwrap();
        assert_eq!(sell_row_a.get("issueDate").and_then(|v| v.as_i64()), Some(sell_issue_ft));
        assert_eq!(sell_row_a.get("duration").and_then(|v| v.as_i64()), Some(90));
        assert_eq!(sell_row_a.get("accountID").and_then(|v| v.as_i64()), Some(1000));
        assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "CharGetTransactions", &[]).await.unwrap(), char_tx_before);
        assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(1000)]).await.unwrap(), corp_tx_before);
        assert_eq!(rpc(&ctx2, &mut s1_re, "account", "GetTransactions", &[PyRep::Integer(1000)]).await.unwrap(), acct_tx_before);
        assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "billMgr", "GetCorporationBills", &[]).await.unwrap()).contains(&bill_id.to_string()));
        assert!(rpc(&ctx2, &mut s1_re, "billMgr", "GetAutomaticPaySettings", &[]).await.is_ok());
        assert_eq!(rpc(&ctx2, &mut s1_re, "trademgr", "List", &[PyRep::Integer(trade_id)]).await.unwrap().dict_get("myMoney").and_then(|v| v.as_f64()), Some(125_000.0));
        assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.unwrap()).contains(&raffle_id.to_string()));
        ctx2.db.with_snapshot(|s| {
            assert_eq!(s.wallet_journal.len(), wj_before);
            assert_eq!(s.char_wallet_transactions.len(), wt_before);
        });
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod mail_no_global_cap_state {
    // End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 2).
    //
    // Covers:
    // - `feat:lifecycle.mail-no-global-cap`, `feat:lifecycle.mail-state-persist`,
    //   `feat:lifecycle.notifications-durable`
    // - `feat:lifecycle.market_order-expiry-trigger`, `feat:lifecycle.lp-durable`
    // - `feat:lifecycle.jetcan-empty`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`,
    //   `feat:lifecycle.module-cycles-stop-on-leave-space`, `feat:lifecycle.mission-downtime-respawn`


    use crate::common;
    use eve_db::{
        corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord,
        IskCents, MailRecord, MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord,
        VirtualServerClock, WorldRepository,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::with_combat_runtime;
    use eve_server::services::wallet_serverops::run_daily_downtime_and_jobs;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const CORP_ID: i64 = 2001;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const AMARR_SYSTEM_ID: u32 = 30_002_187;
    const AMARR_REGION_ID: u32 = 10_000_043;
    const AMARR_STATION_ID: i64 = 60_008_494;

    fn temp_db_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_b30_p2_{tag}_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_fresh_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
        repo.with_mut_snapshot(|s| *s = demo);
        let ctx = ServerContext::with_repository(Arc::new(repo));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx
    }

    fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
        let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
        let ctx = ServerContext::with_repository(Arc::new(repo));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
    }

    async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
        ctx.advance_secs(12);
        sess
    }

    fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
    }

    fn insert_expired_order(snap: &mut eve_db::WorldStateSnapshot, oid: i64, owner: i64, tid: u32, is_bid: bool, price: IskCents, qty: i32, escrow: IskCents, now_u: u64) {
        snap.market_orders.insert(oid, MarketOrderRecord {
            order_id: oid, owner_id: owner, station_id: AMARR_STATION_ID, region_id: AMARR_REGION_ID,
            type_id: tid, is_bid, price, vol_entered: qty, vol_remaining: qty, min_volume: 1, is_npc: false, escrow,
        });
        let nid = -(4_000_000_000_000_i64.saturating_add(oid.abs()));
        snap.owner_notes.insert(nid, OwnerNoteRecord {
            note_id: nid, character_id: -4, item_id: oid, label: format!("MarketOrderMeta:{oid}"),
            note: format!("{{\"range\":32767,\"duration\":1,\"issue_unix\":{}}}", now_u.saturating_sub(86_500)), timestamp_unix: now_u,
        });
    }

    #[tokio::test]
    async fn test_b30_mail_no_global_cap_state_and_notifications_durability() {
        // parity: feat:lifecycle.mail-no-global-cap, feat:lifecycle.mail-state-persist, feat:lifecycle.notifications-durable
        eve_testkit::attest(&[
            "feat:lifecycle.mail-no-global-cap",
            "feat:lifecycle.mail-state-persist",
            "feat:lifecycle.notifications-durable",
        ]);

        let db_path = temp_db_path("mail_notif");
        let base_u = 1_760_000_000u64;
        let ctx = open_fresh_ctx_at(&db_path, base_u);

        ctx.db.with_mut_snapshot(|snap| {
            for i in 1..=3_001i64 {
                snap.mails.push(MailRecord {
                    message_id: 100_000 + i, sender_id: P1, recipient_id: 80_000_000 + (i % 50),
                    subject: format!("Bulk {i}"), body: "Body".into(),
                });
            }
        });

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P2) {
                ch.corporation_id = CORP_ID;
            }
            if let Some(c) = snap.corporations.get_mut(&CORP_ID) {
                c.members.insert(P2);
                c.alliance_id = Some(99_000_001);
            }
            snap.alliances.insert(99_000_001, eve_db::AllianceRecord {
                alliance_id: 99_000_001, name: "B30 Alliance".into(), short_name: "B30A".into(),
                executor_corp_id: CORP_ID, member_corp_ids: [CORP_ID].into_iter().collect(), ..Default::default()
            });
        });

        let mut s1 = login_select(&ctx, 9301, "demo", "demo-secret", P1).await;
        s1.corporation_id = Some(CORP_ID);
        s1.alliance_id = Some(99_000_001);
        let ml_rep = rpc(&ctx, &mut s1, "mailingListsMgr", "Create", &[PyRep::String("B30 Dispatch".into()), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(250)]).await.unwrap();
        let list_id = ml_rep.as_i64().unwrap();
        rpc(&ctx, &mut s1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome".into()), PyRep::String("Body".into())]).await.unwrap();

        let mail_3002 = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
            PyRep::String("Mail 3002".into()), PyRep::String("Delivered past 3001".into()),
            PyRep::Integer(0), PyRep::Integer(0),
        ]).await.unwrap().as_i64().unwrap();
        let mail_corp = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::Integer(CORP_ID),
            PyRep::String("Corp Mail".into()), PyRep::String("Corp body".into()),
            PyRep::Integer(0), PyRep::Integer(0),
        ]).await.unwrap().as_i64().unwrap();
        let mail_ally = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::Integer(99_000_001),
            PyRep::String("Ally Mail".into()), PyRep::String("Ally body".into()),
            PyRep::Integer(0), PyRep::Integer(0),
        ]).await.unwrap().as_i64().unwrap();
        let mail_trash = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
            PyRep::String("Trash Mail".into()), PyRep::String("Trash".into()),
            PyRep::Integer(0), PyRep::Integer(0),
        ]).await.unwrap().as_i64().unwrap();
        let mail_del = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
            PyRep::String("Del Mail".into()), PyRep::String("Del".into()),
            PyRep::Integer(0), PyRep::Integer(0),
        ]).await.unwrap().as_i64().unwrap();

        let notif_p1 = ctx.create_character_notification(1_000_125, P1, 10, 1, "B30 P1 processed notif").notification_id;
        rpc(&ctx, &mut s1, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(notif_p1)])]).await.unwrap();
        let notif_offline = ctx.create_character_notification(1_000_125, P2, 14, 2, "B30 offline notif").notification_id;

        let mut s2 = login_select(&ctx, 9302, "other", "other-secret", P2).await;
        let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
        assert!(format!("{sync_rep:?}").contains(&mail_3002.to_string()));
        rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::String("B30 Dispatch".into())]).await.unwrap();
        rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.unwrap();
        let lbl_id = rpc(&ctx, &mut s2, "mailMgr", "CreateLabel", &[PyRep::String("B30 Intel".into()), PyRep::Integer(4)]).await.unwrap().as_i64().unwrap_or(64);
        rpc(&ctx, &mut s2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(mail_3002)]), PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(lbl_id)])]).await.unwrap();
        rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_3002)])]).await.unwrap();
        rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_trash)])]).await.unwrap();
        rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_del)])]).await.unwrap();
        rpc(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[
            PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60),
            PyRep::String("B30 Formup".into()), PyRep::String("Ops".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(P2)]),
        ]).await.unwrap();

        let (read_b, trash_b, del_b, lbl_b, ml_b, cal_b) = ctx.db.with_snapshot(|s| (
            s.read_mails.clone(), s.trashed_mails.clone(), s.deleted_mails.clone(),
            s.mail_assigned_labels.clone(), s.mailing_lists.clone(), s.calendar_events.clone(),
        ));
        drop(ctx);

        let ctx2 = open_ctx_at(&db_path, base_u + 100);
        let mut s1_re = login_select(&ctx2, 9303, "demo", "demo-secret", P1).await;
        let p1_notifs = rpc(&ctx2, &mut s1_re, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
        assert!(p1_notifs.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_p1)) && r.dbrow_get("processed") == Some(&PyRep::Bool(true))));

        let mut s2_re = login_select(&ctx2, 9304, "other", "other-secret", P2).await;
        s2_re.corporation_id = Some(CORP_ID);
        s2_re.alliance_id = Some(99_000_001);
        let p2_login_notifs = ctx2.drain_notifications_for_char(P2);
        assert!(p2_login_notifs.iter().any(|n| n.event == "OnNotificationReceived" && n.args.first() == Some(&PyRep::Integer(notif_offline))));
        let p2_unproc = rpc(&ctx2, &mut s2_re, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
        assert!(p2_unproc.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_offline))));
        let notif_after = ctx2.create_character_notification(1_000_125, P2, 14, 2, "B30 post-restart").notification_id;
        assert!(notif_after > notif_offline);

        // Verify mail headers, labels, mailing lists, and calendar events via RPCs post-restart
        let hdrs_s = format!("{:?}", rpc(&ctx2, &mut s2_re, "mailMgr", "GetMailHeaders", &[PyRep::List(vec![PyRep::Integer(mail_3002), PyRep::Integer(mail_corp), PyRep::Integer(mail_ally)])]).await.unwrap());
        assert!(hdrs_s.contains(&mail_3002.to_string()) && hdrs_s.contains(&mail_corp.to_string()) && hdrs_s.contains(&mail_ally.to_string()));
        assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "mailMgr", "GetLabels", &[]).await.unwrap()).contains("B30 Intel"));
        assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "mailingListsMgr", "GetJoinedLists", &[]).await.unwrap()).contains(&list_id.to_string()));
        assert!(rpc(&ctx2, &mut s2_re, "mailingListsMgr", "GetWelcomeMail", &[PyRep::Integer(list_id)]).await.is_ok());
        assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "calendarMgr", "GetEventList", &[PyRep::Integer(5), PyRep::Integer(2030)]).await.unwrap()).contains("B30 Formup"));

        ctx2.db.with_snapshot(|s| {
            assert_eq!(s.read_mails, read_b);
            assert_eq!(s.trashed_mails, trash_b);
            assert_eq!(s.deleted_mails, del_b);
            assert_eq!(s.mail_assigned_labels, lbl_b);
            assert_eq!(s.mailing_lists.len(), ml_b.len());
            assert_eq!(s.calendar_events.len(), cal_b.len());
        });
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b30_market_order_expiry_and_lp_durability() {
        // parity: feat:lifecycle.market_order-expiry-trigger, feat:lifecycle.lp-durable
        eve_testkit::attest(&[
            "feat:lifecycle.market_order-expiry-trigger",
            "feat:lifecycle.lp-durable",
        ]);

        let db_path = temp_db_path("market_lp");
        let base_u = 1_760_000_000u64;
        let ctx = open_fresh_ctx_at(&db_path, base_u);
        let (p2_bid, p2_ask) = (910_004i64, 910_005i64);

        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID);
            let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_TRADER;
            let mut corp = CorporationRecord {
                corporation_id: CORP_ID, name: "B30 Market Corp".into(), ticker: "B30M".into(), ceo_id: P1,
                hq_station_id: Some(AMARR_STATION_ID), members: [P1].into_iter().collect(),
                member_roles: HashMap::from([(P1, roles)]), ..Default::default()
            };
            corp.ensure_wallet_divisions();
            snap.corporations.insert(CORP_ID, corp);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.corporation_id = CORP_ID;
                ch.balance = IskCents(10_000_000_00);
                ch.station_id = Some(AMARR_STATION_ID);
                ch.solar_system_id = AMARR_SYSTEM_ID;
                ch.region_id = AMARR_REGION_ID;
                ch.loyalty_points.insert(1_000_035, 15_000);
                ch.loyalty_points.insert(1_000_036, 500);
                ch.loyalty_points.insert(1_000_125, 5_000);
            }
            snap.items.insert(720_001, make_item(720_001, 36, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 100, false));
            snap.items.insert(720_002, make_item(720_002, 34, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 50, false));
            snap.items.insert(720_003, make_item(720_003, 35, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 50, false));
            insert_expired_order(snap, p2_bid, P2, 36, true, IskCents(500_00), 40, IskCents(20_000_00), base_u);
            insert_expired_order(snap, p2_ask, P2, 36, false, IskCents(50_00), 40, IskCents(0), base_u);
        });

        let mut s1 = login_select(&ctx, 9401, "demo", "demo-secret", P1).await;
        // Place P1 buy & sell orders with duration=1, then advance clock 86_500s to expire naturally
        let _buy_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(AMARR_STATION_ID), PyRep::Integer(35), PyRep::Real(100.0), PyRep::Integer(100),
            PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Bool(false),
        ]).await.unwrap();
        let sell_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(720_003)), (PyRep::String("typeID".into()), PyRep::Integer(35)),
            (PyRep::String("price".into()), PyRep::Real(200.0)), (PyRep::String("quantity".into()), PyRep::Integer(50)),
            (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(1)),
        ])]), PyRep::Bool(false)]).await.unwrap().dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        let bal_escrowed = ctx.db.get_character(P1).unwrap().balance.0;
        let _ = ctx.drain_notifications_for_char(P1);
        ctx.advance_secs(86_500);
        // All 5 read RPCs exclude expired orders
        for m in ["GetRegionBest", "GetStationAsks", "GetCharOrders", "GetCorporationOrders"] {
            let rep = rpc(&ctx, &mut s1, "marketProxy", m, &[]).await.unwrap();
            assert!(!format!("{rep:?}").contains(&p2_ask.to_string()));
            assert!(!format!("{rep:?}").contains(&sell_oid.to_string()));
        }
        let ord_rep = rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[PyRep::Integer(36)]).await.unwrap();
        assert!(!format!("{ord_rep:?}").contains(&p2_bid.to_string()));

        // Refunded escrow + returned 50 units of type 35 to AMARR_STATION_ID hangar + OnMarketItemReceived
        let bal_after_exp = ctx.db.get_character(P1).unwrap().balance.0;
        assert_eq!(bal_after_exp - bal_escrowed, 10_000_00);
        ctx.db.with_snapshot(|s| {
            let pyerite_qty: i32 = s.items.values()
                .filter(|i| i.owner_id == P1 && i.location_id == AMARR_STATION_ID && i.flag == InventoryFlag::Hangar && i.type_id == 35)
                .map(|i| i.quantity)
                .sum();
            assert_eq!(pyerite_qty, 50);
        });
        let _ = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap();
        assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_after_exp);
        let notifs = ctx.drain_notifications_for_char(P1);
        let item_recv = notifs.iter().find(|n| n.event == "OnMarketItemReceived").unwrap();
        assert_eq!(item_recv.args.last(), Some(&PyRep::Integer(AMARR_STATION_ID)));

        // All 3 match RPCs never match expired orders
        let bal_before = ctx.db.get_character(P1).unwrap().balance.0;
        let buy_m = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(AMARR_STATION_ID), PyRep::Integer(36), PyRep::Real(100.0), PyRep::Integer(10), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false)]).await;
        assert!(buy_m.is_err() || buy_m.as_ref().ok().and_then(|r| r.dict_get("filledImmediately")).and_then(|v| v.as_bool()).unwrap_or(false) == false);
        let sell_err = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(720_001)), (PyRep::String("typeID".into()), PyRep::Integer(36)),
            (PyRep::String("price".into()), PyRep::Real(100.0)), (PyRep::String("quantity".into()), PyRep::Integer(10)),
            (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(0)),
        ])]), PyRep::Bool(false)]).await.unwrap_err();
        assert_eq!(sell_err.code, "ImmediateOrderUnfilled");
        let _ = rpc(&ctx, &mut s1, "marketProxy", "BuyMultipleItems", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(100.0)])])]).await;
        assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_before);

        // LP durability + logs + atomic failed purchase
        rpc(&ctx, &mut s1, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(CORP_ID), PyRep::Integer(1_000_035), PyRep::Integer(6_000)]).await.unwrap();
        rpc(&ctx, &mut s1, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(1_000_036), PyRep::Integer(1_000)]).await.unwrap();
        let corp_lp_before = rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
        let logs_before = rpc(&ctx, &mut s1, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
        drop(ctx);

        let ctx2 = open_ctx_at(&db_path, base_u + 100);
        common::seed_test_lp_store_offers(&ctx2);
        let mut s1_re = login_select(&ctx2, 9402, "demo", "demo-secret", P1).await;
        let cn_station = ctx2.sde.stations.values().find(|s| s.owner_corporation_id == 1_000_035 && eve_server::services::station_structure::npc_station_offers_service(&ctx2.sde, s.station_id, 25)).unwrap().station_id;
        ctx2.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(cn_station); } });
        s1_re.station_id = Some(cn_station);
        assert_eq!(rpc(&ctx2, &mut s1_re, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap(), corp_lp_before);
        assert_eq!(rpc(&ctx2, &mut s1_re, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap(), logs_before);

        let snap_before = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
        assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err().code, "MissingRequiredItems");
        assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1003), PyRep::Integer(1)]).await.unwrap_err().code, "InsufficientLoyaltyPoints");
        let snap_after = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
        assert_eq!(snap_after, snap_before);
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_b30_space_jetcan_mwd_module_cycles_and_mission_downtime() {
        // parity: feat:lifecycle.jetcan-empty, feat:lifecycle.mobile_warp_disruptor-lifecycle, feat:lifecycle.module-cycles-stop-on-leave-space, feat:lifecycle.mission-downtime-respawn
        eve_testkit::attest(&[
            "feat:lifecycle.jetcan-empty",
            "feat:lifecycle.mobile_warp_disruptor-lifecycle",
            "feat:lifecycle.module-cycles-stop-on-leave-space",
            "feat:lifecycle.mission-downtime-respawn",
        ]);

        let db_path = temp_db_path("space");
        let day_start = 1_800_000_000u64 - (1_800_000_000u64 % 86_400);
        let base_u = day_start + 11 * 3_600 - 5;
        let ctx = open_fresh_ctx_at(&db_path, base_u);
        let sys_id = JITA_SYSTEM_ID;
        let (ship_id, cargo_a, cargo_b, mod_id, gun_id, mwd_id) = (620_000_001i64, 620_000_010i64, 620_000_011i64, 620_000_020i64, 620_000_021i64, 620_000_030i64);

        let sys = ctx.systems.get_or_spawn(sys_id);
        let station_pos = ctx.sde.stations[&JITA_STATION_ID].position;
        let ship_ball = Ball { id: ship_id, type_id: 639, position: station_pos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 100_000_000.0, radius: 250.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        sys.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 639), ctx.build_ship_slim_meta(P1, 639)).await.unwrap();

        let sec_agent = ctx.sde.agents.values().find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1 && a.station_id == Some(JITA_STATION_ID)).cloned()
            .or_else(|| ctx.sde.agents.values().find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1 && a.station_id.is_some()).cloned()).unwrap();
        let ag_station = sec_agent.station_id.unwrap();
        let ag_sys = sec_agent.solar_system_id.unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.solar_system_id = sys_id; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
                ch.standings.insert(sec_agent.agent_id, 9.0);
                ch.standings.insert(sec_agent.corporation_id, 9.0);
            }
            for stid in [11_584_u32, 3_435_u32, 3_436_u32, 3_300_u32, 3_301_u32, 3_311_u32] {
                snap.skills.entry(P1).or_default().insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
            }
            snap.items.insert(ship_id, make_item(ship_id, 639, P1, sys_id as i64, InventoryFlag::None, 1, true));
            snap.items.insert(cargo_a, make_item(cargo_a, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
            snap.items.insert(cargo_b, make_item(cargo_b, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
            snap.items.insert(mod_id, make_item(mod_id, 3528, P1, ship_id, InventoryFlag::LoSlot0, 1, true));
            for (i, flg) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2].into_iter().enumerate() {
                let gid = gun_id + (i as i64) * 2;
                snap.items.insert(gid, make_item(gid, 2929, P1, ship_id, flg, 1, true));
                snap.items.insert(gid + 1, make_item(gid + 1, 193, P1, ship_id, flg, 500, false));
                snap.module_online_states.insert(gid, true);
            }
            snap.items.insert(mwd_id, make_item(mwd_id, 12_198, P1, ship_id, InventoryFlag::Cargo, 1, true));
        });

        let mut sess = SessionState { user_id: Some(1), session_id: 9501, character_id: Some(P1), ship_id: Some(ship_id), solar_system_id: Some(sys_id), ..Default::default() };
        let _ = ctx.db.acquire_online_lock(1, 9501, Some(P1), false, base_u);

        // 1. `jetcan-empty`: last item removed deletes container item, ball, and broadcasts RemoveBall
        let jet_res = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_a), PyRep::Integer(cargo_b)])]).await.unwrap();
        let can_id = match &jet_res { PyRep::Tuple(t) => t.first().and_then(|v| match v { PyRep::List(l) => l.first().and_then(|x| x.as_i64()), _ => None }).unwrap(), _ => panic!() };
        let _ = sys.step_manual(1.0).await;
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_a), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
        assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_some());
        ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_b), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
        ctx.db.with_snapshot(|s| assert!(!s.items.contains_key(&can_id)));
        assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_none());
        assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "DoDestinyUpdate" && format!("{:?}", n.args).contains("RemoveBall")));

        // 2. `module-cycles-stop-on-leave-space`: Activate repeater, verify active cycle, Dock, verify cycle removed & deactivation emitted
        let act_rep = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("repair".into())]).await.unwrap();
        assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
        ctx.db.with_snapshot(|s| assert!(s.module_active_states.contains(&mod_id)));
        let _ = sys.step_manual(1.0).await.unwrap();
        assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(true));
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
        assert_eq!(sess.station_id, Some(JITA_STATION_ID));
        ctx.db.with_snapshot(|s| assert!(!s.module_active_states.contains(&mod_id)));
        assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(false));
        assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnGodmaShipEffect"));

        // 3. `mission-downtime-respawn`: Accept real mission, warp in, kill NPC via dogmaIM, downtime resets & respawns on warp-in
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(ag_station); ch.solar_system_id = ag_sys; }
            if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = ag_station; }
        });
        sess.station_id = Some(ag_station);
        sess.solar_system_id = Some(ag_sys);
        rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(sec_agent.agent_id)]).await.unwrap();
        let m_id = ctx.db.with_snapshot(|s| s.active_missions.keys().find_map(|&(c, mid)| (c == P1).then_some(mid)).unwrap());
        rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(sec_agent.agent_id), PyRep::Integer(i64::from(m_id))]).await.unwrap();
        let m_sys = ctx.db.with_snapshot(|s| s.active_missions[&(P1, m_id)].solar_system_id);
        let ag_space = ctx.systems.get_or_spawn(m_sys);
        if m_sys == ag_sys {
            let _ = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
        } else {
            ctx.db.with_mut_snapshot(|s| {
                if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = None; ch.solar_system_id = m_sys; }
                if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = i64::from(m_sys); }
            });
            sess.station_id = None;
            sess.solar_system_id = Some(m_sys);
            ag_space.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
        }
        let ag_beyonce = ctx.bind_object(&sess, "beyonce", i64::from(m_sys)).unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(i64::from(m_id))], &HashMap::new(), None).await.unwrap();
        for _ in 0..120 {
            ag_space.step_manual(1.0).await.unwrap();
            if matches!(ag_space.get_ball_snapshot(ship_id).await.unwrap(), Some((_, b)) if b.mode == MovementMode::Stop) {
                break;
            }
        }
        let (initial_npcs, first_npc, initial_npc_ids) = ctx.db.with_snapshot(|s| (
            s.active_missions[&(P1, m_id)].npcs_remaining,
            s.active_missions[&(P1, m_id)].spawned_npc_ball_ids[0],
            s.active_missions[&(P1, m_id)].spawned_npc_ball_ids.clone(),
        ));
        let mut initial_types = Vec::with_capacity(initial_npc_ids.len());
        for nid in &initial_npc_ids {
            initial_types.push(ag_space.get_ball_snapshot(*nid).await.unwrap().unwrap().1.type_id);
        }
        let npc_pos = ag_space.get_ball_snapshot(first_npc).await.unwrap().unwrap().1.position;
        let ship_pos = ag_space.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
        if ship_pos.distance(npc_pos) > 1_000.0 {
            let _ = ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdStop", &[], &HashMap::new(), None).await;
            assert!(ag_space.set_ball_position(ship_id, npc_pos + DVec3::new(500.0, 0.0, 0.0)).await);
        }
        rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(first_npc)]).await.unwrap();
        for _ in 0..25 {
            if ag_space.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == first_npc && l.locked) { break; }
            ag_space.step_manual(1.0).await.unwrap();
        }
        for gid in [gun_id, gun_id + 2, gun_id + 4] {
            rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(first_npc)]).await.expect("Activate launcher on first_npc");
        }
        for _ in 0..480 {
            if ag_space.get_ball_snapshot(first_npc).await.unwrap().is_none() { break; }
            ag_space.step_manual(1.0).await.unwrap();
        }
        assert!(ag_space.get_ball_snapshot(first_npc).await.unwrap().is_none(), "first_npc destroyed before downtime");
        ctx.db.with_snapshot(|s| assert_eq!(s.active_missions[&(P1, m_id)].npcs_remaining, initial_npcs - 1));
        ctx.advance_secs(10);
        run_daily_downtime_and_jobs(&ctx);
        ctx.db.with_snapshot(|s| assert_eq!(s.active_missions[&(P1, m_id)].npcs_remaining, initial_npcs));
        let ag_beyonce = ctx.bind_object(&sess, "beyonce", i64::from(m_sys)).unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(i64::from(m_id))], &HashMap::new(), None).await.unwrap();
        let respawned_ids = ctx.db.with_snapshot(|s| s.active_missions[&(P1, m_id)].spawned_npc_ball_ids.clone());
        assert_eq!(respawned_ids.len() as u32, initial_npcs);
        assert!(respawned_ids.iter().all(|&id| id >= 700_000_000));
        let mut respawned_types = Vec::with_capacity(respawned_ids.len());
        for nid in &respawned_ids {
            respawned_types.push(ag_space.get_ball_snapshot(*nid).await.unwrap().unwrap().1.type_id);
        }
        assert_eq!(respawned_types, initial_types);
        assert!(with_combat_runtime(&ctx, |rt| respawned_ids.iter().all(|id| rt.npc_entities.contains_key(id))));

        // 4. `mobile_warp_disruptor-lifecycle`: Anchor completion from anchor_start_unix, bubble re-created after restart, removed after 2 days
        let null_sys_id = 30_004_608u32;
        let null_sys = ctx.systems.get_or_spawn(null_sys_id);
        null_sys.spawn_ball_with_meta(Ball { id: ship_id, type_id: 587, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = null_sys_id; });
        sess.solar_system_id = Some(null_sys_id);
        rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mwd_id), PyRep::Tuple(vec![PyRep::Real(50_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.unwrap();
        ctx.advance_secs(120);
        assert_eq!(null_sys.is_ball_scrambled(ship_id).await.unwrap(), true);
        let mwd_u = ctx.now_unix();
        drop(ctx);

        let ctx2 = open_ctx_at(&db_path, mwd_u + 60);
        let null_sys2 = ctx2.systems.get_or_spawn(null_sys_id);
        assert!(null_sys2.get_ball_snapshot(mwd_id).await.unwrap().is_some());
        null_sys2.spawn_ball_with_meta(Ball { id: ship_id, type_id: 587, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx2.build_ship_combat_state(P1, ship_id, 587), ctx2.build_ship_slim_meta(P1, 587)).await.unwrap();
        assert_eq!(null_sys2.is_ball_scrambled(ship_id).await.unwrap(), true);
        ctx2.advance_secs(172_900);
        run_daily_downtime_and_jobs(&ctx2);
        ctx2.db.with_snapshot(|s| assert!(!s.items.contains_key(&mwd_id)));
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod foreman_bursts_harvester_validity {
    // End-to-end integration tests for Batch 33 open `feat:mining.*` rows (part 1: Foreman bursts,
    // harvester validity, phased asteroid field lifecycle, Mobile Phase Anchor, Prismaticite &
    // Unrefined Mineral reactions, and wormhole ore sites).


    use std::collections::{HashMap, HashSet};

    use crate::mining_cycle_engine::{
        activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR,
    };
    use eve_db::{CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::PyRep;
    use glam::DVec3;

    const MERCOXIT: u32 = 11_396;
    const BLUE_ICE: u32 = 16_264;
    const FULLERITE_C50: u32 = 30_370;
    const DEEP_CORE_MINER_I: u32 = 12_108;
    const DEEP_CORE_STRIP_II: u32 = 24_305;
    const STRIP_MINER_I: u32 = 17_482;
    const MOD_STRIP_MINER_II: u32 = 17_912;
    const HULK: u32 = 22_544;
    const MLU_II: u32 = 28_576;
    const ICE_HARVESTER_I: u32 = 16_278;
    const GAS_HARVESTER_I: u32 = 25_266;
    const ICE_HARVESTER_UPGRADE_II: u32 = 28_578;
    const ICE_ACCEL_RIG_I: u32 = 32_819;
    const YETI_IMPLANT_1005: u32 = 22_571;
    const FOREMAN_MINDLINK: u32 = 22_559;
    const LARGE_IND_CORE_I: u32 = 58_945;
    const BURST_II: u32 = 43_551;
    const CHARGE_FIELD: u32 = 42_829;
    const CHARGE_OPTIMIZATION: u32 = 42_830;
    const CHARGE_PRESERVATION: u32 = 42_831;
    const CHARGE_EFFICIENCY: u32 = 90_733;
    const SURVEY_SCANNER_I: u32 = 444;
    const BURST_I: u32 = 42_528;
    const PORPOISE: u32 = 42_244;
    const ORCA: u32 = 28_606;
    const ROKH: u32 = 24_688;
    const PRISMATICITE: u32 = 90_041;
    const COMPRESSED_PRISMATICITE: u32 = 90_307;
    const ATMOSPHERIC_GASES: u32 = 16_634;
    const UNREFINED_TRITANIUM: u32 = 90_283;
    const UNREFINED_TRIT_FORMULA: u32 = 90_274;
    const STANDUP_COMPOSITE_REACTOR: u32 = 45_537;
    const ATHANOR: u32 = 35_835;
    const MOBILE_PHASE_ANCHOR: u32 = 90_037;
    const NULL_SYS: u32 = 30_004_759; // 1DQ1-A

    pub fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn insert_item(
        ctx: &eve_server::router::ServerContext,
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        qty: i32,
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
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    }

    #[tokio::test]
    async fn test_b33_foreman_bursts_and_harvester_validity() {
        // parity: feat:mining.foreman-burst-charges, feat:mining.foreman-burst-charge-effects, feat:mining.ice-harvester-cycle-modifiers, feat:mining.mercoxit-deep-core-restriction, feat:mining.mining-module-target-validity
        eve_testkit::attest(&[
            "feat:mining.foreman-burst-charges",
            "feat:mining.foreman-burst-charge-effects",
            "feat:mining.ice-harvester-cycle-modifiers",
            "feat:mining.mercoxit-deep-core-restriction",
            "feat:mining.mining-module-target-validity",
        ]);

        let (miner_id, strip_id, strip2_id, deep_core_id, deep_strip_id, ice_harv_id, gas_harv_id, burst_id, ind_core_id) = (
            9_100_001_i64, 9_100_003_i64, 9_100_004_i64, 9_100_005_i64,
            9_100_006_i64, 9_100_007_i64, 9_100_008_i64, 9_100_009_i64, 9_100_010_i64,
        );

        // 0. Command Burst command-hull enforcement: Rokh battleship must be rejected on both fitting and activation
        let mut p_rokh = pilot(
            ROKH,
            &[(22_536, 5), (22_552, 5), (3_426, 5), (3_413, 5)],
            &[(9_100_099, BURST_II, InventoryFlag::HiSlot0)],
        )
        .await;
        insert_item(&p_rokh.ctx, 9_100_098, CHARGE_FIELD, CHAR_ID, p_rokh.ship_id, InventoryFlag::HiSlot0, 2);
        let rokh_fit = p_rokh.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_rokh.ship_id).unwrap();
        assert!(!rokh_fit.modules.contains_key(&9_100_099), "Dogma fitting must reject Mining Foreman Burst II on Rokh");
        let rokh_act_err = rpc(&mut p_rokh, "dogmaIM", None, "Activate", &[PyRep::Integer(9_100_099)]).await.unwrap_err();
        assert!(rokh_act_err.contains("CantFitToShip"), "Activate must reject Mining Foreman Burst II on Rokh: {rokh_act_err}");

        let mut p = pilot(
            ORCA,
            &[
                (3_386, 5), (3_410, 5), (11_395, 5), (16_281, 5), (25_544, 5),
                (22_536, 5), (22_552, 5), (29_637, 5), (33_399, 5), (3_429, 5), (3_426, 5), (3_413, 5),
            ],
            &[
                (miner_id, MINER_I, InventoryFlag::HiSlot0),
                (burst_id, BURST_II, InventoryFlag::HiSlot1),
                (ind_core_id, LARGE_IND_CORE_I, InventoryFlag::HiSlot2),
            ],
        )
        .await;
        p.ctx.db.with_mut_snapshot(|s| {
            s.active_implants.entry(CHAR_ID).or_default().push(FOREMAN_MINDLINK);
            s.module_active_states.insert(ind_core_id);
        });
        let bid = p.beyonce_id;
        let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

        // 1. Ice harvester cycle duration modifiers: skill, non-stacking Ice Harvester Upgrade II, Hulk hull, rig, implant (1 block/cycle)
        let (ice_upg_1, ice_upg_2, ice_rig_id) = (9_100_021_i64, 9_100_022_i64, 9_100_023_i64);
        let mut p_ice = pilot(
            HULK,
            &[(3_386, 5), (11_395, 5), (16_281, 5), (17_940, 5), (22_551, 5), (22_552, 5), (22_578, 4), (3_429, 5), (3_426, 5), (3_413, 5)],
            &[
                (ice_harv_id, ICE_HARVESTER_I, InventoryFlag::HiSlot0),
                (ice_upg_1, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot0),
                (ice_upg_2, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot1),
                (ice_rig_id, ICE_ACCEL_RIG_I, InventoryFlag::RigSlot0),
            ],
        )
        .await;
        p_ice.ctx.db.with_mut_snapshot(|s| {
            for mid in [ice_harv_id, ice_upg_1, ice_upg_2, ice_rig_id] { s.module_online_states.insert(mid, true); }
            s.active_implants.entry(CHAR_ID).or_default().push(YETI_IMPLANT_1005);
        });
        p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
        let fit_ice = p_ice.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_ice.ship_id).unwrap();
        let base_ice_dur = p_ice.ctx.sde.types.get(&ICE_HARVESTER_I).unwrap().default_attributes[&73];
        let ice_dur = fit_ice.get_module_effective_attribute(ice_harv_id, 73, &p_ice.ctx.sde);
        assert!(ice_dur < base_ice_dur * 0.60, "Ice Harvester duration ({ice_dur}) must be reduced by skill+2xUpgrade+Hulk+rig+Yeti from {base_ice_dur}");
        p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(ice_upg_2, false); });
        p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
        let ice_dur_1_upg = p_ice.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_ice.ship_id).unwrap().get_module_effective_attribute(ice_harv_id, 73, &p_ice.ctx.sde);
        assert!(ice_dur < ice_dur_1_upg, "Second Ice Harvester Upgrade II (stackable=1) must further reduce duration ({ice_dur} < {ice_dur_1_upg})");
        p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(ice_upg_2, true); });
        p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
        let ice_rock_id = 9_200_099_i64;
        rock(&p_ice, ice_rock_id, BLUE_ICE, 100, 500.0).await;
        lock(&mut p_ice, ice_rock_id).await;
        activate(&mut p_ice, ice_harv_id, ice_rock_id).await.unwrap();
        let ice_out = advance(&p_ice, 200).await;
        assert!(!ice_out.is_empty() && ice_out.iter().all(|o| o.units_delivered == 1), "Each ice harvester cycle must yield 1 block");

        // 2. Target validity & Mercoxit Deep Core restriction (including Miner II, Strip Miners, Deep Core Strip Miner II, and positive gas harvesting)
        let (veld_rock, merc_rock, ice_rock, gas_cloud, far_rock) = (9_200_001, 9_200_002, 9_200_003, 9_200_004, 9_200_005);
        for (rid, tid, qty, dist) in [
            (veld_rock, VELDSPAR, 10_000, 500.0), (merc_rock, MERCOXIT, 10_000, 500.0),
            (ice_rock, BLUE_ICE, 100, 500.0), (gas_cloud, FULLERITE_C50, 1_000, 500.0), (far_rock, VELDSPAR, 10_000, 35_000.0),
        ] {
            rock(&p, rid, tid, qty, dist).await;
            lock(&mut p, rid).await;
        }
        assert!(activate(&mut p, miner_id, merc_rock).await.is_err(), "Miner I must be refused on Mercoxit");
        assert!(activate(&mut p, miner_id, ice_rock).await.is_err());
        assert!(activate(&mut p, miner_id, gas_cloud).await.is_err());
        assert!(activate(&mut p, miner_id, far_rock).await.is_err());

        let mut p_dc = pilot(
            ROKH,
            &[(3_386, 5), (11_395, 5), (25_544, 5), (3_429, 5), (3_426, 5), (3_413, 5)],
            &[(deep_core_id, DEEP_CORE_MINER_I, InventoryFlag::HiSlot0), (gas_harv_id, GAS_HARVESTER_I, InventoryFlag::HiSlot1)],
        )
        .await;
        let bid_dc = p_dc.beyonce_id;
        let _ = rpc(&mut p_dc, "beyonceBound", Some(bid_dc), "CmdStop", &[]).await;
        p_dc.ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(deep_core_id, true);
            s.module_online_states.insert(gas_harv_id, true);
        });
        p_dc.ctx.invalidate_ship_fitting(p_dc.ship_id);
        let (dc_veld_rock, dc_merc_rock, dc_gas_cloud) = (9_200_101_i64, 9_200_102_i64, 9_200_103_i64);
        for (rid, tid, qty) in [(dc_veld_rock, VELDSPAR, 10_000), (dc_merc_rock, MERCOXIT, 10_000), (dc_gas_cloud, FULLERITE_C50, 1_000)] {
            rock(&p_dc, rid, tid, qty, 100.0).await;
            lock(&mut p_dc, rid).await;
        }
        assert!(activate(&mut p_dc, gas_harv_id, dc_veld_rock).await.is_err());
        activate(&mut p_dc, deep_core_id, dc_merc_rock).await.unwrap();
        let _ = rpc(&mut p_dc, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_core_id)]).await;

        // Positive gas-cloud harvesting
        activate(&mut p_dc, gas_harv_id, dc_gas_cloud).await.unwrap();
        let gas_out = advance(&p_dc, 45).await;
        assert!(!gas_out.is_empty() && gas_out[0].units_delivered > 0, "Gas Cloud Harvester must harvest Fullerite-C50");
        let _ = rpc(&mut p_dc, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gas_harv_id)]).await;

        let hulk_merc_id = 9_200_098_i64;
        rock(&p_ice, hulk_merc_id, MERCOXIT, 10_000, 500.0).await;
        lock(&mut p_ice, hulk_merc_id).await;
        assert!(activate(&mut p_ice, ice_harv_id, hulk_merc_id).await.is_err(), "Ice Harvester must be refused on ore");
        p_ice.ctx.db.with_mut_snapshot(|s| {
            s.items.remove(&ice_harv_id);
            s.module_online_states.remove(&ice_harv_id);
        });
        for (bad_mod, bad_tid) in [(strip_id, STRIP_MINER_I), (strip2_id, MOD_STRIP_MINER_II)] {
            insert_item(&p_ice.ctx, bad_mod, bad_tid, CHAR_ID, p_ice.ship_id, InventoryFlag::HiSlot0, 1);
            p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(bad_mod, true); });
            p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
            assert!(activate(&mut p_ice, bad_mod, hulk_merc_id).await.is_err(), "Non-Deep-Core strip {bad_mod} must be refused on Mercoxit");
            p_ice.ctx.db.with_mut_snapshot(|s| { s.items.remove(&bad_mod); });
        }
        insert_item(&p_ice.ctx, deep_strip_id, DEEP_CORE_STRIP_II, CHAR_ID, p_ice.ship_id, InventoryFlag::HiSlot0, 1);
        p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(deep_strip_id, true); });
        p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
        activate(&mut p_ice, deep_strip_id, hulk_merc_id).await.unwrap();
        let _ = rpc(&mut p_ice, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_strip_id)]).await;

        // 3. Fleet setup with populated member_details (wing/squad), in-range (5 km), out-of-range (120 km), and non-fleet bystander (3 km)
        let (in_range_cid, in_range_sid, in_range_miner) = (9_210_001_i64, 9_210_002_i64, 9_210_010_i64);
        let (out_range_cid, out_range_sid) = (9_210_003_i64, 9_210_004_i64);
        let (non_fleet_cid, non_fleet_sid) = (9_210_005_i64, 9_210_006_i64);
        p.ctx.db.with_mut_snapshot(|snap| {
            let base_ch = snap.characters.get(&CHAR_ID).unwrap().clone();
            let base_skills = snap.skills.get(&CHAR_ID).cloned().unwrap_or_default();
            for (cid, sid) in [(in_range_cid, in_range_sid), (out_range_cid, out_range_sid), (non_fleet_cid, non_fleet_sid)] {
                let mut ch = base_ch.clone();
                ch.character_id = cid;
                ch.active_ship_id = sid;
                snap.characters.insert(cid, ch);
                snap.skills.insert(cid, base_skills.clone());
                snap.items.insert(sid, InventoryItemRecord {
                    item_id: sid, type_id: ROKH, owner_id: cid, location_id: i64::from(JITA),
                    flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
                });
            }
            snap.items.insert(in_range_miner, InventoryItemRecord {
                item_id: in_range_miner, type_id: MINER_I, owner_id: in_range_cid, location_id: in_range_sid,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(in_range_miner, true);
            let mut fl = FleetRecord { fleet_id: 77_001, boss_id: CHAR_ID, members: vec![CHAR_ID, in_range_cid, out_range_cid], ..Default::default() };
            fl.member_details.insert(CHAR_ID, eve_db::FleetMemberRecord {
                character_id: CHAR_ID, role: eve_db::FleetRole::FleetCommander, wing_id: Some(1), squad_id: Some(1),
                takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
                job: 2, join_time_unix: 0, role_name: "FC".into(),
            });
            fl.member_details.insert(in_range_cid, eve_db::FleetMemberRecord {
                character_id: in_range_cid, role: eve_db::FleetRole::SquadMember, wing_id: Some(1), squad_id: Some(1),
                takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
                job: 0, join_time_unix: 0, role_name: String::new(),
            });
            fl.member_details.insert(out_range_cid, eve_db::FleetMemberRecord {
                character_id: out_range_cid, role: eve_db::FleetRole::SquadMember, wing_id: Some(1), squad_id: Some(1),
                takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
                job: 0, join_time_unix: 0, role_name: String::new(),
            });
            snap.fleets.insert(77_001, fl);
        });
        let sys_handle = p.ctx.systems.get_or_spawn(JITA);
        for (sid, offset_x) in [(in_range_sid, 5_000.0), (out_range_sid, 120_000.0), (non_fleet_sid, 3_000.0)] {
            sys_handle.spawn_ball(Ball {
                id: sid, type_id: HULK, position: p.ship_pos + DVec3::new(offset_x, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            }).await.unwrap();
        }

        let miner_type = p.ctx.sde.types.get(&MINER_I).unwrap().clone();
        let dbuff = |ctx: &eve_server::router::ServerContext, sid: i64, attr: u16| {
            eve_server::services::mining_cycle::dbuff_multiplier(ctx, sid, &miner_type, attr)
        };

        // 4a. Field Enhancement charge: boosts range (54) on Miner/Strip/Ice/Survey Scanner without squaring; timed expiry
        let survey_id = 9_100_033_i64;
        insert_item(&p.ctx, survey_id, SURVEY_SCANNER_I, CHAR_ID, p.ship_id, InventoryFlag::MedSlot0, 1);
        p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(survey_id, true); });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let charge_item_id = 9_100_030_i64;
        insert_item(&p.ctx, charge_item_id, CHARGE_FIELD, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 8);
        let burst_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
        assert_eq!(dict_get(&burst_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_FIELD))));
        let buff_dur = dict_get(&burst_rep, "buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!((buff_dur - 112.5).abs() < 1e-4, "Mining Foreman V + Mindlink must scale burst duration to 112.5s, got {buff_dur}");
        assert_eq!(p.ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).map(|i| i.quantity).unwrap_or(0)), 7);

        let range_mult = dbuff(&p.ctx, in_range_sid, 54);
        assert!(range_mult > 2.0 && range_mult < 3.5, "In-range fleet member must receive Field Enhancement range boost ({range_mult})");
        assert!((dbuff(&p.ctx, out_range_sid, 54) - 1.0).abs() < 1e-6, "Out-of-range ship must not receive burst");
        assert!((dbuff(&p.ctx, non_fleet_sid, 54) - 1.0).abs() < 1e-6, "Non-fleet bystander must not receive burst");
        let strip_t = p.ctx.sde.types.get(&STRIP_MINER_I).unwrap();
        let ice_t = p.ctx.sde.types.get(&ICE_HARVESTER_I).unwrap();
        assert!((eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, in_range_sid, strip_t, 54) - range_mult).abs() < 1e-6);
        assert!((eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, in_range_sid, ice_t, 54) - range_mult).abs() < 1e-6);

        // Survey Scanner range uses single range_mult (not squared)
        let survey_t = p.ctx.sde.types.get(&SURVEY_SCANNER_I).unwrap();
        let survey_range_mult = eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, p.ship_id, survey_t, 54);
        let survey_near = 9_200_201_i64;
        let survey_too_far = 9_200_202_i64;
        rock(&p, survey_near, VELDSPAR, 5_000, 15_000.0 * survey_range_mult * 0.90).await;
        rock(&p, survey_too_far, VELDSPAR, 5_000, 15_000.0 * survey_range_mult * 1.15).await;
        let scan_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(survey_id)]).await.unwrap();
        let scanned_ids: Vec<i64> = dict_get(&scan_rep, "asteroids").and_then(|v| match v {
            PyRep::List(items) => Some(items.iter().filter_map(|e| dict_get(e, "asteroidID").and_then(|x| x.as_i64())).collect()),
            _ => None,
        }).unwrap_or_default();
        assert!(scanned_ids.contains(&survey_near) && !scanned_ids.contains(&survey_too_far), "Survey Scanner must boost range once without squaring: {scanned_ids:?}");

        // Timed buff expiry check
        p.ctx.clock().advance_secs(60);
        assert!((dbuff(&p.ctx, in_range_sid, 54) - range_mult).abs() < 1e-6, "Buff must remain active before expires_at_unix");
        p.ctx.clock().advance_secs(60);
        assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6, "Buff must expire after buffDurationSeconds");

        // 4b. Isolated strength scaling: T1 vs T2 (+25%), Mining Director (+10%/lvl), Mindlink (+25%), Industrial Core (+10%)
        let full_str = dict_get(&burst_rep, "buffStrength").and_then(|v| v.as_f64()).unwrap();
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&ind_core_id); });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let no_core_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
        assert!((full_str / no_core_str - 1.10).abs() < 1e-4, "Large Ind Core I must scale strength by 1.10x");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { s.active_implants.remove(&CHAR_ID); });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let no_mind_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
        assert!((no_core_str / no_mind_str - 1.25).abs() < 1e-4, "Foreman Mindlink must scale strength by 1.25x");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { if let Some(sk) = s.skills.get_mut(&CHAR_ID).and_then(|m| m.get_mut(&22_552)) { sk.level = 1; } });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let dir1_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
        assert!((no_mind_str / dir1_str - (1.5 / 1.1)).abs() < 1e-4, "Mining Director V vs I must scale by 1.5/1.1");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&burst_id) { it.type_id = BURST_I; }
            if let Some(sk) = s.skills.get_mut(&CHAR_ID).and_then(|m| m.get_mut(&22_552)) { sk.level = 5; }
            s.active_implants.entry(CHAR_ID).or_default().push(FOREMAN_MINDLINK);
            s.module_active_states.insert(ind_core_id);
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let t1_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
        assert!((full_str / t1_str - 1.25).abs() < 1e-4, "T2 Burst must scale strength by 1.25x over T1 Burst");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { if let Some(it) = s.items.get_mut(&burst_id) { it.type_id = BURST_II; } s.items.remove(&charge_item_id); });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        p.ctx.clock().advance_secs(120);

        // 4b. Optimization charge: reduces duration (73) & capacitorNeed (6) on live mining cycle; no range/volatility
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&charge_item_id); });
        p.ctx.clock().advance_secs(120);
        let mut sess_m = p.sess.clone();
        sess_m.character_id = Some(in_range_cid);
        sess_m.ship_id = Some(in_range_sid);
        let _ = sys_handle.try_update_combat_state(in_range_sid, |c| { c.capacitor = 1_000.0; c.max_capacitor = 1_000.0; });
        p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "AddTarget", &[PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
        for _ in 0..30 {
            if sys_handle.get_target_locks(in_range_sid).await.unwrap().iter().any(|l| l.target_id == veld_rock && l.locked) { break; }
            sys_handle.step_manual(1.0).await.unwrap();
        }
        let cap_before_unboosted = sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
        p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Activate", &[PyRep::Integer(in_range_miner), PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
        let unboosted_cap_used = cap_before_unboosted - sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
        let _ = p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Deactivate", &[PyRep::Integer(in_range_miner)], &HashMap::new(), None).await;

        insert_item(&p.ctx, 9_100_031, CHARGE_OPTIMIZATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
        assert!(dbuff(&p.ctx, in_range_sid, 73) < 0.70, "Optimization + Mindlink + IndCore must reduce duration >= 30%");
        assert!(dbuff(&p.ctx, in_range_sid, 6) < 0.70, "Optimization + Mindlink + IndCore must reduce cap use >= 30%");
        assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6);

        let cap_before_boosted = sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
        p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Activate", &[PyRep::Integer(in_range_miner), PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
        let boosted_cap_used = cap_before_boosted - sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
        assert!(boosted_cap_used < unboosted_cap_used * 0.75, "Boosted mining cycle cap ({boosted_cap_used}) must be reduced vs unboosted ({unboosted_cap_used})");

        // 4c. Efficiency charge (90733): boosts crit chance (5967) & lowers residue (3154); Preservation (42831): lowers volatility (783)
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_031); });
        p.ctx.clock().advance_secs(120);
        insert_item(&p.ctx, 9_100_032, CHARGE_EFFICIENCY, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
        assert!(dbuff(&p.ctx, in_range_sid, 5967) > 1.5, "Efficiency charge must boost mining crit chance (5967)");
        assert!(dbuff(&p.ctx, in_range_sid, 3154) < 0.70, "Efficiency charge must reduce mining residue probability (3154)");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_032); });
        p.ctx.clock().advance_secs(120);
        insert_item(&p.ctx, 9_100_034, CHARGE_PRESERVATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
        assert!(dbuff(&p.ctx, in_range_sid, 783) < 0.70, "Preservation charge must reduce crystal volatility >= 30%");
        assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6);
        assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_b33_phased_asteroid_field_mobile_anchor_and_prismaticite() {
        // parity: feat:mining.phased-asteroid-field-lifecycle, feat:mining.mobile-phase-anchor, feat:mining.prismaticite-reprocessing-and-unrefined-reactions, feat:mining.wormhole-ore-sites
        eve_testkit::attest(&[
            "feat:mining.phased-asteroid-field-lifecycle",
            "feat:mining.mobile-phase-anchor",
            "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
            "feat:mining.wormhole-ore-sites",
        ]);

        let miner_id = 9_300_001_i64;
        let mut p = pilot(
            HULK,
            &[
                (3_386, 5),  // Mining V
                (3_410, 5),  // Astrogeology V
                (17_940, 5), // Mining Barge V
                (22_551, 5), // Exhumers V
                (22_578, 4), // Mining Upgrades IV
                (32_918, 5), // Mining Frigate V
                (3_380, 5),  // Industry V
                (45_746, 5), // Reactions V
                (3_327, 5),  // Spaceship Command V
                (3_429, 5),  // Target Management V
                (3_426, 5),  // CPU Management V
            ],
            &[
                (miner_id, STRIP_MINER_I, InventoryFlag::HiSlot0),
                (9_300_002, MLU_II, InventoryFlag::LoSlot0),
                (9_300_003, MLU_II, InventoryFlag::LoSlot1),
                (9_300_004, MLU_II, InventoryFlag::LoSlot2),
            ],
        )
        .await;
        let bid = p.beyonce_id;
        let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

        // 1. High-sec Jita refuses Phased Asteroid Field; null-sec 1DQ1-A spawns probeable signature
        assert!(
            eve_server::services::mining_phased_fields::spawn_phased_asteroid_field(
                &p.ctx, JITA, "PHS-HI", 9_300_009, 10_000, (0.0, 0.0, 0.0),
            )
            .is_none(),
            "Phased Asteroid Field must not spawn in high-sec"
        );

        let ast_pos = DVec3::new(250_000.0, 12_000.0, -180_000.0);
        p.ship_pos = ast_pos - DVec3::new(1_000.0, 0.0, 0.0);
        p.sess.solar_system_id = Some(NULL_SYS);
        p.ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.solar_system_id = NULL_SYS;
            }
            if let Some(sh) = snap.items.get_mut(&p.ship_id) {
                sh.location_id = i64::from(NULL_SYS);
            }
        });
        let null_sys_handle = p.ctx.systems.get_or_spawn(NULL_SYS);
        null_sys_handle
            .spawn_ball(Ball {
                id: p.ship_id,
                type_id: HULK,
                position: p.ship_pos,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();

        let pris_ast_id = 9_300_010_i64;
        let field = eve_server::services::mining_phased_fields::spawn_phased_asteroid_field(
            &p.ctx,
            NULL_SYS,
            "PHS-001",
            pris_ast_id,
            50_000,
            (ast_pos.x, ast_pos.y, ast_pos.z),
        )
        .expect("Null-sec Phased Asteroid Field must spawn");
        assert!(!field.beacon_spawned && !field.phased_in);
        null_sys_handle
            .spawn_asteroid(
                eve_server::system_actor::AsteroidState {
                    asteroid_id: pris_ast_id, belt_id: 0, type_id: PRISMATICITE,
                    initial_units: 50_000, remaining_units: 50_000, unit_volume: 16.0, depleted: false, respawn_at_tick: None,
                },
                Ball {
                    id: pris_ast_id, type_id: PRISMATICITE, position: ast_pos, velocity: DVec3::ZERO,
                    max_velocity: 0.0, inertia: 1.0, mass: 1e9, radius: 250.0, speed_fraction: 0.0, mode: MovementMode::Stop,
                },
            )
            .await
            .unwrap();
        let other_pris_id = 9_300_011_i64;
        p.ctx.db.with_mut_snapshot(|s| {
            s.asteroids.insert(other_pris_id, eve_db::AsteroidRecord {
                asteroid_id: other_pris_id, solar_system_id: NULL_SYS, belt_id: 0, type_id: PRISMATICITE,
                quantity: 25_000, initial_quantity: 25_000, position: (ast_pos.x + 5_000.0, ast_pos.y, ast_pos.z), radius: 250.0,
            });
        });

        // Breached Prismaticite mines at 10% efficiency before Mobile Phase Anchor is deployed & powered
        rpc(&mut p, "dogmaIM", None, "AddTarget", &[PyRep::Integer(pris_ast_id)]).await.expect("AddTarget");
        for _ in 0..30 {
            if null_sys_handle.get_target_locks(p.ship_id).await.unwrap().iter().any(|l| l.target_id == pris_ast_id && l.locked) {
                break;
            }
            null_sys_handle.step_manual(1.0).await.unwrap();
        }
        activate(&mut p, miner_id, pris_ast_id).await.unwrap();
        let breached_outcomes = advance(&p, 46).await;
        let breached_units = breached_outcomes[0].units_delivered;
        assert!(breached_units > 0);

        // 2. Deploy Mobile Phase Anchor (90037) via ship.Drop; reject WH/Pochven; live tick reconciles ICS ships within 50 km
        let (porp1_ship, orca_ship, porp2_ship) = (9_300_031_i64, 9_300_032_i64, 9_300_033_i64);
        assert!(!eve_server::services::mining_phased_fields::is_low_or_null_kspace(&p.ctx, 31_000_005), "Wormhole space must reject Mobile Phase Anchor");
        assert!(!eve_server::services::mining_phased_fields::is_low_or_null_kspace(&p.ctx, 30_001_445), "Pochven must reject Mobile Phase Anchor");

        insert_item(&p.ctx, 9_300_019, MOBILE_PHASE_ANCHOR, 9_999_999, i64::from(NULL_SYS), InventoryFlag::None, 1);
        let anchor_cargo_id = 9_300_020_i64;
        insert_item(&p.ctx, anchor_cargo_id, MOBILE_PHASE_ANCHOR, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1);
        let _ = rpc(&mut p, "ship", None, "Drop", &[PyRep::Integer(anchor_cargo_id)]).await.unwrap();

        p.ctx.db.with_mut_snapshot(|snap| {
            for (idx, (cid, sid, tid)) in [(9_300_131_i64, porp1_ship, PORPOISE), (9_300_132_i64, orca_ship, ORCA), (9_300_133_i64, porp2_ship, PORPOISE)].into_iter().enumerate() {
                let mut ch = snap.characters.get(&CHAR_ID).unwrap().clone();
                ch.character_id = cid;
                ch.name = format!("ICS_Pilot_{idx}");
                ch.active_ship_id = sid;
                ch.solar_system_id = NULL_SYS;
                snap.characters.insert(cid, ch);
                snap.items.insert(sid, InventoryItemRecord {
                    item_id: sid, type_id: tid, owner_id: cid, location_id: i64::from(NULL_SYS),
                    flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
                });
            }
            snap.fleets.insert(9_300_200, FleetRecord {
                fleet_id: 9_300_200, boss_id: CHAR_ID, members: vec![CHAR_ID, 9_300_131, 9_300_132, 9_300_133], ..Default::default()
            });
        });
        for (sid, tid, offset) in [(porp1_ship, PORPOISE, 2_000.0), (orca_ship, ORCA, 4_000.0)] {
            null_sys_handle.spawn_ball(Ball {
                id: sid, type_id: tid, position: p.ship_pos + DVec3::new(offset, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            }).await.unwrap();
        }
        null_sys_handle.spawn_ball(Ball {
            id: porp2_ship, type_id: PORPOISE, position: p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        // Without active Industrial Cores, ICS hulls in range do NOT connect
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        assert!((eve_server::services::mining_phased_fields::phased_field_efficiency_for_asteroid(&p.ctx, NULL_SYS, pris_ast_id, PRISMATICITE) - 0.10).abs() < 1e-6);

        // Activating Industrial Cores on Porpoise #1 + Orca + Porpoise #2 connects (35 + 50 + 35 = 120 >= 100)
        p.ctx.db.with_mut_snapshot(|s| {
            for (idx, sid) in [porp1_ship, orca_ship, porp2_ship].into_iter().enumerate() {
                let cid = 9_300_300 + idx as i64;
                s.items.insert(cid, InventoryItemRecord { item_id: cid, type_id: LARGE_IND_CORE_I, owner_id: CHAR_ID, location_id: sid, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
                s.module_active_states.insert(cid);
            }
        });
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        assert!((eve_server::services::mining_phased_fields::phased_field_efficiency_for_asteroid(&p.ctx, NULL_SYS, pris_ast_id, PRISMATICITE) - 1.0).abs() < 1e-6);

        // Moving Porpoise #2 beyond 50 km tether range drops energy to 85 < 100 and reverts phased_in
        assert!(null_sys_handle.set_ball_position(porp2_ship, p.ship_pos + DVec3::new(60_000.0, 0.0, 0.0)).await);
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        assert!((eve_server::services::mining_phased_fields::phased_field_efficiency_for_ore(&p.ctx, NULL_SYS, PRISMATICITE) - 0.10).abs() < 1e-6);
        assert!(null_sys_handle.set_ball_position(porp2_ship, p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0)).await);
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;

        let phased_outcomes = advance(&p, 46).await;
        let phased_units = phased_outcomes[0].units_delivered;
        assert!(phased_units >= breached_units * 8, "Phased-in Prismaticite ({phased_units}) must yield ~10x breached yield ({breached_units})");

        // 3. Live cluster tick advances fracture past 0.5 -> beacon spawns at site position; past 1.0 -> field collapses
        p.ctx.clock().advance_secs(1);
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        p.ctx.clock().advance_secs(7_201);
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        let anom_pos = p.ctx.db.with_snapshot(|s| {
            s.cosmic_anomalies.get(&NULL_SYS).and_then(|v| v.iter().find(|a| a.anomaly_id == "PHS-001" && !a.cleared)).map(|a| a.position)
        });
        assert_eq!(anom_pos, Some([ast_pos.x, ast_pos.y, ast_pos.z]), "Beacon must spawn at actual site position");

        p.ctx.clock().advance_secs(7_201);
        eve_server::services::mining_phased_fields::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
        assert_eq!(p.ctx.db.with_snapshot(|s| s.asteroids.get(&pris_ast_id).map(|a| a.quantity).unwrap_or(-1)), 0);
        assert_eq!(p.ctx.db.with_snapshot(|s| s.asteroids.get(&other_pris_id).map(|a| a.quantity).unwrap_or(-1)), 25_000, "Collapse must not wipe unrelated Prismaticite");

        // 4. Unrefined Mineral reaction (requires online Standup Composite Reactor I 45537) & live reprocessingSvc.Reprocess variable batches
        let (athanor_id, bp_id) = (1_000_000_099_001_i64, 9_300_200_i64);
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(9_800_099, CorporationRecord {
                corporation_id: 9_800_099, name: "React Corp".into(), ticker: "RCT".into(), ceo_id: CHAR_ID, ..Default::default()
            });
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.corporation_id = 9_800_099;
                ch.balance = eve_db::IskCents(100_000_000_000);
                ch.structure_id = Some(athanor_id);
            }
            snap.structures.insert(athanor_id, UpwellStructureRecord {
                structure_id: athanor_id, type_id: ATHANOR, owner_corp_id: 9_800_099, solar_system_id: NULL_SYS,
                name: "React Athanor".into(), position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0, ..Default::default()
            });
            snap.items.insert(bp_id, InventoryItemRecord {
                item_id: bp_id, type_id: UNREFINED_TRIT_FORMULA, owner_id: CHAR_ID, location_id: athanor_id,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        });
        p.sess.structure_id = Some(athanor_id);
        insert_item(&p.ctx, 9_300_202, ATMOSPHERIC_GASES, CHAR_ID, athanor_id, InventoryFlag::Hangar, 1_000);
        insert_item(&p.ctx, 9_300_203, COMPRESSED_PRISMATICITE, CHAR_ID, athanor_id, InventoryFlag::Hangar, 1_000);
        insert_item(&p.ctx, 9_300_204, 35_899, 9_800_099, athanor_id, InventoryFlag::ServiceSlot1, 1);
        p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(9_300_204, true); });

        let react_err = rpc(&mut p, "industryManager", None, "InstallJob", &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(5)]).await.unwrap_err();
        assert!(react_err.contains("CompositeReactorRequired"), "Unexpected error: {react_err}");

        insert_item(&p.ctx, 9_300_201, STANDUP_COMPOSITE_REACTOR, 9_800_099, athanor_id, InventoryFlag::ServiceSlot0, 1);
        p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(9_300_201, true); });
        let job_rep = rpc(&mut p, "industryManager", None, "InstallJob", &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(5)]).await.expect("Reaction InstallJob must succeed");
        let job_id = dict_get(&job_rep, "jobID").and_then(|v| v.as_i64()).expect("jobID");
        p.ctx.clock().advance_secs(3_600);
        let _ = rpc(&mut p, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)]).await.expect("CompleteJob must deliver Unrefined Tritanium");
        let produced_unref: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == UNREFINED_TRITANIUM).map(|i| i.quantity).sum());
        assert!(produced_unref >= 100, "Completed reaction must produce Unrefined Tritanium, got {produced_unref}");

        // Reprocess multiple batches of Prismaticite & produced Unrefined Tritanium via live reprocessingSvc.Reprocess
        let (mut seen_minerals, mut pris_qtys, mut unref_qtys) = (HashSet::new(), HashSet::new(), HashSet::new());
        let hangar_minerals = |ctx: &eve_server::router::ServerContext| {
            ctx.db.with_snapshot(|s| {
                let mut m = HashMap::new();
                for it in s.items.values().filter(|i| i.location_id == athanor_id && ((34..=40).contains(&i.type_id) || i.type_id == 11_399)) {
                    *m.entry(it.type_id).or_insert(0) += it.quantity;
                }
                m
            })
        };
        for batch in 0..8i64 {
            let pris_batch_id = 9_300_300 + batch;
            let unref_batch_id = 9_300_400 + batch;
            insert_item(&p.ctx, pris_batch_id, PRISMATICITE, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
            insert_item(&p.ctx, unref_batch_id, UNREFINED_TRITANIUM, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
            let before_minerals = hangar_minerals(&p.ctx);
            let _ = rpc(&mut p, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(pris_batch_id)]).await.unwrap();
            for (tid, qty) in hangar_minerals(&p.ctx) {
                let delta = qty - before_minerals.get(&tid).copied().unwrap_or(0);
                if delta > 0 { seen_minerals.insert(tid); pris_qtys.insert(delta); }
            }
            let trit_before: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == 34).map(|i| i.quantity).sum());
            let _ = rpc(&mut p, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(unref_batch_id)]).await.unwrap();
            let trit_after: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == 34).map(|i| i.quantity).sum());
            unref_qtys.insert(trit_after - trit_before);
        }
        assert!(seen_minerals.len() >= 3, "Prismaticite reprocessing across batches must yield random minerals from the 8 mineral types");
        assert!(pris_qtys.len() >= 2 && unref_qtys.len() >= 3, "Prismaticite and Unrefined Mineral reprocessing must yield variable batch quantities");

        // 5. Wormhole ore sites by class
        let c1_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(1);
        let c5_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(5);
        assert!(c1_sites.iter().all(|s| !s.dungeon_name.contains("Core")));
        assert!(c5_sites.iter().any(|s| s.dungeon_name.contains("Core")));
    }
}

pub(crate) mod moon_mining_reprocessing_curves {
    // End-to-end integration tests for Batch 33 open `feat:mining.*` and `feat:modules.*` rows (part 2:
    // moon extraction duration/yield, auto-fracture, moon ore class by security, Upwell refinery
    // base yield, NPC reprocessing tax curve, Scrapmetal exclusions, ARSB/ARAR, and Auto Targeting System).


    use std::collections::HashMap;

    use crate::mining_cycle_engine::{lock, pilot, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR};
    use crate::corp_mining_lifecycle_rpcs::foreman_bursts_harvester_validity::{dict_get, insert_item};
    use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::PyRep;
    use glam::DVec3;

    const PORPOISE: u32 = 42_244;
    const RORQUAL: u32 = 28_352;
    const ATHANOR: u32 = 35_835;
    const TATARA: u32 = 35_836;
    const ASTRAHUS: u32 = 35_832;
    const RAITARU: u32 = 35_825;
    const MOON_DRILL_I: u32 = 35_894;
    const MOON_STABILITY_RIG_I: u32 = 46_325;
    const REPROC_MONITOR_RIG_II: u32 = 46_640;
    const RX_804_IMPLANT: u32 = 27_174;
    const ARAR_MEDIUM: u32 = 41_477;
    const ARSB_MEDIUM: u32 = 41_481;
    const AUTO_TARGETING_SYS_I: u32 = 1_182;
    const AUTO_TARGETING_SYS_II: u32 = 1_436;
    const ANGEL_ROGUE_NPC: u32 = 2_372;
    const NULL_SYS: u32 = 30_004_759; // 1DQ1-A
    const WH_SYS: u32 = 31_000_005;

    #[tokio::test]
    async fn test_b33_moon_mining_and_reprocessing_curves() {
        // parity: feat:mining.moon-extraction-duration-yield, feat:mining.moon-chunk-auto-fracture, feat:mining.moon-ore-class-by-security, feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.upwell-refinery-base-yield
        eve_testkit::attest(&[
            "feat:mining.moon-extraction-duration-yield",
            "feat:mining.moon-chunk-auto-fracture",
            "feat:mining.moon-ore-class-by-security",
            "feat:mining.npc-reprocessing-equipment-and-tax-curve",
            "feat:mining.scrapmetal-yield-exclusions",
            "feat:mining.upwell-refinery-base-yield",
        ]);

        let p = pilot(PORPOISE, &[(12_196, 5), (3_359, 5)], &[]).await;
        let kw = HashMap::new();
        let mut sess = p.sess.clone();

        let low_sys = p.ctx.sde.solar_systems.values()
            .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.05 && s.security_status < 0.45
                && p.ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 8))
            .map(|s| s.solar_system_id)
            .unwrap_or(30_002_510);
        let wh_sys = p.ctx.sde.solar_systems.values()
            .find(|s| (31_000_000..=32_999_999).contains(&s.solar_system_id)
                && p.ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 8))
            .map(|s| s.solar_system_id)
            .unwrap_or(WH_SYS);
        let low_npc_station = p.ctx.sde.solar_systems.values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.reprocessing_efficiency >= 0.30 && st.reprocessing_efficiency < 0.49)
            .map(|st| (st.station_id, st.reprocessing_efficiency))
            .expect("NPC station with < 50% reprocessing efficiency");

        let corp_id = 9_800_001_i64;
        let (athanor_id, tatara_id, null_athanor_id, low_athanor_id, wh_athanor_id, raitaru_id, astrahus_id, null_tatara_id) = (
            1_000_000_001_001_i64, 1_000_000_001_002_i64, 1_000_000_001_003_i64, 1_000_000_001_004_i64,
            1_000_000_001_005_i64, 1_000_000_001_006_i64, 1_000_000_001_007_i64, 1_000_000_001_008_i64,
        );
        let now = p.ctx.now_unix_secs();
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id, name: "B33 Mining Corp".into(), ticker: "B33M".into(), ceo_id: CHAR_ID, ..Default::default()
            });
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) { ch.corporation_id = corp_id; }
            for (sid, tid, sys_id, pos) in [
                (athanor_id, ATHANOR, JITA, [100_000.0, 0.0, 0.0]),
                (tatara_id, TATARA, JITA, [200_000.0, 0.0, 0.0]),
                (null_athanor_id, ATHANOR, NULL_SYS, [300_000.0, 0.0, 0.0]),
                (low_athanor_id, ATHANOR, low_sys, [400_000.0, 0.0, 0.0]),
                (wh_athanor_id, ATHANOR, wh_sys, [500_000.0, 0.0, 0.0]),
                (raitaru_id, RAITARU, JITA, [600_000.0, 0.0, 0.0]),
                (astrahus_id, ASTRAHUS, JITA, [700_000.0, 0.0, 0.0]),
                (null_tatara_id, TATARA, NULL_SYS, [800_000.0, 0.0, 0.0]),
            ] {
                snap.structures.insert(sid, UpwellStructureRecord {
                    structure_id: sid, type_id: tid, owner_corp_id: corp_id, solar_system_id: sys_id,
                    name: format!("B33 Refinery {sid}"), position: pos, state: "ShieldVulnerable".into(),
                    quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
                    ..Default::default()
                });
            }
            for (iid, tid, loc, flag) in [
                (9_400_001, MOON_DRILL_I, athanor_id, InventoryFlag::ServiceSlot0),
                (9_400_002, 35_899, athanor_id, InventoryFlag::ServiceSlot1),
                (9_400_003, 35_899, tatara_id, InventoryFlag::ServiceSlot0),
                (9_400_004, MOON_DRILL_I, null_athanor_id, InventoryFlag::ServiceSlot0),
                (9_400_005, MOON_DRILL_I, low_athanor_id, InventoryFlag::ServiceSlot0),
                (9_400_006, MOON_DRILL_I, wh_athanor_id, InventoryFlag::ServiceSlot0),
                (9_400_007, MOON_DRILL_I, tatara_id, InventoryFlag::ServiceSlot1),
                (9_400_008, MOON_STABILITY_RIG_I, tatara_id, InventoryFlag::RigSlot0),
                (9_400_020, 35_899, raitaru_id, InventoryFlag::ServiceSlot0),
                (9_400_021, 35_899, astrahus_id, InventoryFlag::ServiceSlot0),
                (9_400_022, 35_899, null_tatara_id, InventoryFlag::ServiceSlot0),
                (9_400_023, REPROC_MONITOR_RIG_II, null_tatara_id, InventoryFlag::RigSlot0),
            ] {
                snap.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: corp_id, location_id: loc, flag, quantity: 1, singleton: true, version: 1, ..Default::default()
                });
                snap.module_online_states.insert(iid, true);
            }
        });
        sess.corporation_id = Some(corp_id);
        sess.corp_role = u64::MAX;

        // 1. Duration bounds (< 6d and > 56d refused), CancelExtraction produces no chunk
        assert!(
            p.ctx
                .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_399)], &kw, None)
                .await
                .is_err(),
            "Extraction < 6 days must be refused"
        );
        assert!(
            p.ctx
                .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(4_838_401)], &kw, None)
                .await
                .is_err(),
            "Extraction > 56 days must be refused"
        );

        let start_rep = p
            .ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)], &kw, None)
            .await
            .expect("StartNewExtraction 10d");
        assert_eq!(dict_get(&start_rep, "chunkArrivalTime"), Some(&PyRep::Integer((now + 864_000) as i64)));
        let _ = p
            .ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(athanor_id)], &kw, None)
            .await
            .expect("CancelExtraction");
        p.ctx.clock().advance_secs(864_000 + 10_801);
        eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
        let cancelled_spawned = p.ctx.db.with_snapshot(|s| {
            s.moon_extractions.get(&athanor_id).map(|e| e.spawned_asteroid_ids.is_empty()).unwrap_or(true)
        });
        assert!(cancelled_spawned, "CancelExtraction must stop the extraction and produce no chunk");

        // 2. 6-day extraction (4,320,000 m³), early fracture refused, 3h base vs 3.6h rigged auto-fracture, security-band & jackpot ores
        for sid in [athanor_id, tatara_id, null_athanor_id, low_athanor_id, wh_athanor_id] {
            p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(sid), PyRep::Integer(518_400)], &kw, None).await.expect("StartNewExtraction 6d");
        }
        assert!(
            p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)], &kw, None).await.is_err(),
            "FractureChunkWithStructure before chunk arrival must be refused"
        );
        p.ctx.clock().advance_secs(518_400 + 10_801);
        eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
        let rigged_fractured_at_3h = p.ctx.db.with_snapshot(|s| s.moon_extractions.get(&tatara_id).map(|e| e.auto_fracture).unwrap_or(false));
        assert!(!rigged_fractured_at_3h, "Rigged refinery (Moon Drilling Stability I +20%) must not auto-fracture at 3h");

        p.ctx.clock().advance_secs(2_161);
        eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
        let (auto_fractured, rigged_fractured, spawned_ids, chunk_vol_m3, null_ids, low_ids, wh_ids) = p.ctx.db.with_snapshot(|s| {
            let ext = s.moon_extractions.get(&athanor_id).unwrap();
            let vol: f64 = ext.spawned_asteroid_ids.iter().filter_map(|id| s.asteroids.get(id))
                .map(|a| (a.quantity as f64) * p.ctx.sde.types.get(&a.type_id).map(|t| t.volume).unwrap_or(0.0)).sum();
            let ids_of = |k: i64| s.moon_extractions.get(&k).map(|e| e.spawned_asteroid_ids.clone()).unwrap_or_default();
            (ext.auto_fracture, s.moon_extractions.get(&tatara_id).unwrap().auto_fracture, ext.spawned_asteroid_ids.clone(), vol, ids_of(null_athanor_id), ids_of(low_athanor_id), ids_of(wh_athanor_id))
        });
        assert!(auto_fractured && rigged_fractured && !spawned_ids.is_empty());
        assert!((chunk_vol_m3 - 4_320_000.0).abs() < 1_000.0, "6-day chunk volume must be 4,320,000 m³, got {chunk_vol_m3}");
        let only_r4 = |ids: &[i64]| p.ctx.db.with_snapshot(|s| ids.iter().all(|id| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| t.group_id == 1884)));
        assert!(only_r4(&spawned_ids), "High-sec moon must contain only R4 ores");
        assert!(!wh_ids.is_empty() && only_r4(&wh_ids), "Wormhole moon must contain only R4 ores");

        let (null_valid_groups, low_valid_groups, has_improved) = p.ctx.db.with_snapshot(|s| {
            let in_r4_r64 = |id: &i64| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| matches!(t.group_id, 1884 | 1920 | 1921 | 1922 | 1923));
            let imp = null_ids.iter().any(|id| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| t.default_attributes.contains_key(&2699) || t.name.starts_with("Bountiful ") || t.name.starts_with("Glowing ") || t.name.starts_with("Twinkling ") || t.name.starts_with("Scintillating ") || t.name.starts_with("Sparkling ")));
            (null_ids.iter().all(in_r4_r64), low_ids.iter().all(in_r4_r64), imp)
        });
        assert!(null_valid_groups && !null_ids.is_empty() && low_valid_groups && !low_ids.is_empty(), "Low-sec and null-sec moons must contain R4-R64 ores");
        assert!(has_improved, "Fractured moon field must mix basic and improved (+15%) quality ores");

        // Jackpot fracture check
        let mut hit_jackpot = false;
        for _ in 0..40 {
            p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(null_athanor_id), PyRep::Integer(518_400)], &kw, None).await.unwrap();
            p.ctx.clock().advance_secs(518_401);
            let frac = p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(null_athanor_id)], &kw, None).await.unwrap();
            if dict_get(&frac, "isJackpot") == Some(&PyRep::Bool(true)) {
                assert_eq!(dict_get(&frac, "jackpotBonus"), Some(&PyRep::Real(2.0)));
                hit_jackpot = true;
                break;
            }
        }
        assert!(hit_jackpot, "Moon fracture must produce jackpot (+100%) ore variant when jackpot roll hits");

        // 3. Upwell refinery base yield via GetQuotes & Reprocess: Citadel/EC 50%, Athanor 51%, Tatara 52% (+1.5% = 52.75%)
        for (idx, (st_id, exp_base, exp_trit)) in [
            (raitaru_id, 0.50, 200),
            (astrahus_id, 0.50, 200),
            (athanor_id, 0.51, 204),
            (tatara_id, 0.5275, 211),
        ].into_iter().enumerate() {
            let item_id = 9_400_030 + idx as i64;
            insert_item(&p.ctx, item_id, VELDSPAR, CHAR_ID, st_id, InventoryFlag::Hangar, 100);
            sess.station_id = None;
            sess.structure_id = Some(st_id);
            p.ctx.db.with_mut_snapshot(|s| {
                if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.station_id = None; ch.structure_id = Some(st_id); }
            });
            let quote = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
            let q_base = dict_get(&quote, "stationBaseYield").and_then(|v| v.as_f64()).unwrap_or(0.0);
            assert!((q_base - exp_base).abs() < 1e-4, "GetQuotes stationBaseYield at {st_id} expected {exp_base}, got {q_base}");
            let rep = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(item_id)], &kw, None).await.unwrap();
            let r_yield = dict_get(&rep, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
            assert!((r_yield - exp_base).abs() < 1e-4, "Reprocess yieldRate at {st_id} expected {exp_base}, got {r_yield}");
            let trit: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == st_id && i.type_id == 34).map(|i| i.quantity).sum());
            assert_eq!(trit, exp_trit, "100 Veldspar at {st_id} must yield {exp_trit} Tritanium");
        }

        // 4. NPC station equipment (< 50% & 50%) via GetQuotes, tax curve (5% at 0, 2.5% at 3.335, 0% at 6.67, max(personal, corp)), and Connections 0-vs-1 mission gate
        sess.structure_id = None;
        sess.station_id = Some(low_npc_station.0);
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = None; ch.station_id = Some(low_npc_station.0); }
        });
        let low_q = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let low_base = dict_get(&low_q, "stationBaseYield").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!((low_base - low_npc_station.1).abs() < 1e-4 && low_base < 0.50, "NPC station <50% base yield must match SDE: got {low_base}, expected {}", low_npc_station.1);

        sess.station_id = Some(60_003_760);
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = None; ch.station_id = Some(60_003_760); }
            s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
        });
        let q0 = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!((dict_get(&q0, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-4);

        p.ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 3.335); });
        let q_mid = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!((dict_get(&q_mid, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.025).abs() < 1e-3);

        // Corp standing via real corp members' personal standings (0.0 + 10.0 + 10.0 => avg 6.67 => 0% tax)
        p.ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
            for m_id in [9_800_101_i64, 9_800_102_i64] {
                let mut m = s.characters.get(&CHAR_ID).unwrap().clone();
                m.character_id = m_id;
                m.corporation_id = corp_id;
                m.standings.insert(1_000_035, 10.0);
                s.characters.insert(m_id, m);
            }
        });
        let q_corp = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!(dict_get(&q_corp, "stationTaxRate").and_then(|v| v.as_f64()).unwrap().abs() < 1e-4);

        // Connections V applies ONLY after >= 1 completed mission for that corp
        let agent_id = p.ctx.sde.agents.values().find(|a| a.corporation_id == 1_000_035).map(|a| a.agent_id).unwrap_or(3_008_416);
        p.ctx.db.with_mut_snapshot(|s| {
            s.characters.remove(&9_800_101);
            s.characters.remove(&9_800_102);
            s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 5.0);
        });
        let q_no_mission = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let tax_no_mission = dict_get(&q_no_mission, "stationTaxRate").and_then(|v| v.as_f64()).unwrap();
        assert!((tax_no_mission - 0.0125).abs() < 1e-4, "Connections must not apply with 0 completed corp missions, got {tax_no_mission}");

        p.ctx.db.with_mut_snapshot(|s| {
            s.active_missions.insert((CHAR_ID, agent_id as u32), eve_db::ActiveMissionRecord {
                mission_id: 9_900_101, character_id: CHAR_ID, agent_id: i64::from(agent_id),
                solar_system_id: JITA, state: "Completed".into(), current_room_id: 1, npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
        });
        let q_with_mission = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let tax_with_mission = dict_get(&q_with_mission, "stationTaxRate").and_then(|v| v.as_f64()).unwrap();
        assert!((tax_with_mission - 0.005).abs() < 1e-4, "Connections V must apply after >=1 completed corp mission, got {tax_with_mission}");

        // 5. Scrapmetal yield exclusions: ignores Upwell structure, rig, security-band, and implant bonuses (capped at 55%)
        insert_item(&p.ctx, 9_400_012, MINER_I, CHAR_ID, 60_003_760, InventoryFlag::Hangar, 2);
        let r_scrap_npc = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_012)], &kw, None).await.unwrap();
        assert!((dict_get(&r_scrap_npc, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.55).abs() < 1e-4);

        insert_item(&p.ctx, 9_400_013, MINER_I, CHAR_ID, null_tatara_id, InventoryFlag::Hangar, 2);
        sess.station_id = None;
        sess.structure_id = Some(null_tatara_id);
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.station_id = None; ch.structure_id = Some(null_tatara_id); }
            s.active_implants.entry(CHAR_ID).or_default().push(RX_804_IMPLANT);
        });
        let r_scrap_tatara = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_013)], &kw, None).await.unwrap();
        assert!(
            (dict_get(&r_scrap_tatara, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.55).abs() < 1e-4,
            "Scrapmetal reprocessing in rigged null-sec Tatara with RX-804 implant must still cap at 55%"
        );
    }

    #[tokio::test]
    async fn test_b33_ancillary_remote_repairers_and_auto_targeting_system() {
        // parity: feat:modules.ancillary-remote-repairers, feat:modules.auto-targeting-system
        eve_testkit::attest(&[
            "feat:modules.ancillary-remote-repairers",
            "feat:modules.auto-targeting-system",
        ]);

        let arar_id = 9_500_001_i64;
        let arsb_id = 9_500_002_i64;
        let ats_id = 9_500_003_i64;
        let paste_id = 9_500_004_i64;
        let cap_charge_id = 9_500_005_i64;
        let reload_paste_id = 9_500_006_i64;

        let mut p = pilot(
            RORQUAL,
            &[
                (3_429, 4), // Target Management IV
                (3_430, 0), // Advanced Target Management 0
                (3_428, 5), // Long Range Targeting V
                (3_431, 5), // Signature Analysis V
                (3_392, 5), // Mechanics V
                (3_394, 5), // Hull Upgrades V
                (3_416, 5), // Shield Operation V
                (3_422, 5), // Shield Emission Systems V
                (16_069, 5), // Remote Armor Repair Systems V
                (28_374, 5), // Capital Industrial Ships V
            ],
            &[
                (arar_id, ARAR_MEDIUM, InventoryFlag::HiSlot0),
                (arsb_id, ARSB_MEDIUM, InventoryFlag::HiSlot1),
                (ats_id, AUTO_TARGETING_SYS_I, InventoryFlag::HiSlot2),
            ],
        )
        .await;

        insert_item(&p.ctx, paste_id, 28_668, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 8);
        insert_item(&p.ctx, cap_charge_id, 3_200, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 1);
        insert_item(&p.ctx, reload_paste_id, 28_668, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 16);
        p.ctx.invalidate_ship_fitting(p.ship_id);

        // 1. Auto Targeting System I (+2) & II (+3) passive maxLockedTargets; active auto-lock ignores neutral Jita players and locks hostile targets
        let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
        let max_targets = fit.ship.get_attribute(192, &p.ctx.sde);
        assert!((max_targets - 9.0).abs() < 1e-4, "Auto Targeting System I must passively add +2 maxLockedTargets (expected 9, got {max_targets})");

        let ats2_id = 9_500_007_i64;
        insert_item(&p.ctx, ats2_id, AUTO_TARGETING_SYS_II, CHAR_ID, p.ship_id, InventoryFlag::HiSlot3, 1);
        p.ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(ats_id, false);
            s.module_online_states.insert(ats2_id, true);
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let fit2 = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
        let max_targets_2 = fit2.ship.get_attribute(192, &p.ctx.sde);
        assert!((max_targets_2 - 10.0).abs() < 1e-4, "Auto Targeting System II must passively add +3 maxLockedTargets (expected 10, got {max_targets_2})");
        p.ctx.db.with_mut_snapshot(|s| {
            s.items.remove(&ats2_id);
            s.module_online_states.insert(ats_id, true);
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);

        let neutral_ship_id = 9_500_099_i64;
        let hostile_npc_id = 9_500_100_i64;
        p.ctx.db.with_mut_snapshot(|s| {
            s.items.insert(neutral_ship_id, InventoryItemRecord {
                item_id: neutral_ship_id, type_id: PORPOISE, owner_id: 9_500_999, location_id: i64::from(JITA),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        let sys_handle = p.ctx.systems.get_or_spawn(JITA);
        sys_handle.spawn_ball(Ball {
            id: neutral_ship_id, type_id: PORPOISE, position: p.ship_pos + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();

        let ats_neutral = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
        assert_eq!(
            dict_get(&ats_neutral, "lockingCount").and_then(|v| v.as_i64()).unwrap_or(-1),
            0,
            "Auto Targeting System must NOT auto-lock unaggressing neutral player ships in high-sec Jita"
        );
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;

        sys_handle.spawn_ball(Ball {
            id: hostile_npc_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        let ats_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
        assert_eq!(
            dict_get(&ats_rep, "maxLockedTargets").and_then(|v| v.as_i64()).unwrap_or(0),
            6,
            "Auto Targeting System I must obey Target Management IV skill cap (2 + 4 = 6, no double-counted +2)"
        );
        assert_eq!(
            dict_get(&ats_rep, "lockingCount").and_then(|v| v.as_i64()).unwrap_or(0),
            1,
            "Active Auto Targeting System I must automatically initiate target locks on hostile targets"
        );

        // Repeating cycle tick must auto-lock a hostile NPC arriving after activation while ignoring neutral ship & friendly remote repairer
        let late_npc_id = 9_500_101_i64;
        sys_handle.spawn_ball_with_meta(
            Ball {
                id: late_npc_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(7_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            eve_destiny::CombatState { signature_radius: 0.0, ..Default::default() },
            eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
        ).await.unwrap();
        // Friendly remote repairer active on p.ship_id must NOT be auto-locked as hostile
        let friendly_rep_ship = 9_500_102_i64;
        p.ctx.db.with_mut_snapshot(|s| {
            s.items.insert(friendly_rep_ship, InventoryItemRecord {
                item_id: friendly_rep_ship, type_id: PORPOISE, owner_id: 9_500_998, location_id: i64::from(JITA),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        sys_handle.spawn_ball_with_meta(
            Ball {
                id: friendly_rep_ship, type_id: PORPOISE, position: p.ship_pos + DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta { group_id: 941, category_id: 6, owner_id: 9_500_998, ..Default::default() },
        ).await.unwrap();
        sys_handle.add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
            ship_id: friendly_rep_ship, module_id: 9_500_901, module_type_id: ARAR_MEDIUM, owner_char_id: Some(9_500_998),
            effect_name: "remoteArmorRepairFalloff".into(), target_id: Some(p.ship_id), repeat: 1, duration_s: 12.0,
            remote_armor_amount: 100.0, remote_optimal: 10_000.0, remote_falloff: 10_000.0, ..Default::default()
        }).await.unwrap();
        sys_handle.step_manual(11.0).await.unwrap();
        let cycle_locks = sys_handle.get_target_locks(p.ship_id).await.unwrap();
        let late_lock = cycle_locks.iter().find(|l| l.target_id == late_npc_id).expect("Repeating Auto Targeting System cycle must lock late-arriving hostile NPC");
        let npc_sig = p.ctx.sde.types.get(&ANGEL_ROGUE_NPC).and_then(|t| t.default_attributes.get(&552).copied()).filter(|&s| s > 0.0).unwrap_or(50.0);
        let expected_lock_ms = eve_destiny::combat::compute_lock_time_seconds(fit.ship.get_attribute(564, &p.ctx.sde).max(1.0), npc_sig) * 1000.0;
        assert!((late_lock.lock_time_ms - expected_lock_ms).abs() < 1e-3, "ATS must use ship scanResolution & target signatureRadius ({expected_lock_ms}ms), got {}", late_lock.lock_time_ms);
        assert!(!cycle_locks.iter().any(|l| l.target_id == neutral_ship_id), "Repeating Auto Targeting System cycle must not lock neutral player ship");
        assert!(!cycle_locks.iter().any(|l| l.target_id == friendly_rep_ship), "Repeating Auto Targeting System cycle must not lock friendly remote repairer");

        // CONCORD (301) and Gate Sentry Gun (99) on grid must NOT be auto-locked; bloomed CombatState::signature_radius must govern lock time
        let (concord_id, sentry_id, bloomed_rat_id) = (9_500_105_i64, 9_500_106_i64, 9_500_107_i64);
        sys_handle.spawn_ball_with_meta(
            Ball { id: concord_id, type_id: 11_957, position: p.ship_pos + DVec3::new(4_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta { group_id: 301, category_id: 11, owner_id: 500_019, ..Default::default() },
        ).await.unwrap();
        sys_handle.spawn_ball_with_meta(
            Ball { id: sentry_id, type_id: 16_670, position: p.ship_pos + DVec3::new(4_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta { group_id: 99, category_id: 11, owner_id: 500_002, ..Default::default() },
        ).await.unwrap();
        sys_handle.spawn_ball_with_meta(
            Ball { id: bloomed_rat_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(8_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 20.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            eve_destiny::CombatState { signature_radius: 180.0, ..Default::default() },
            eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
        ).await.unwrap();
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
        let jita_locks2 = sys_handle.get_target_locks(p.ship_id).await.unwrap();
        assert!(!jita_locks2.iter().any(|l| l.target_id == concord_id || l.target_id == sentry_id), "ATS must NOT auto-lock CONCORD or Empire Sentry Guns");
        let bloom_lock = jita_locks2.iter().find(|l| l.target_id == bloomed_rat_id).expect("ATS must auto-lock hostile rat");
        let expected_bloom_ms = eve_destiny::combat::compute_lock_time_seconds(fit.ship.get_attribute(564, &p.ctx.sde).max(1.0), 180.0) * 1000.0;
        assert!((bloom_lock.lock_time_ms - expected_bloom_ms).abs() < 1e-3, "ATS must use CombatState::signature_radius (180m), got {}", bloom_lock.lock_time_ms);

        // Null-sec passive neutral player is NOT auto-locked; active offensive aggressor IS auto-locked on initial activation
        let null_sys: u32 = 30_004_759;
        let null_handle = p.ctx.systems.get_or_spawn(null_sys);
        let (null_neutral_id, null_aggressor_id) = (9_500_103_i64, 9_500_104_i64);
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.solar_system_id = null_sys; }
            for (sid, oid) in [(null_neutral_id, 9_500_997_i64), (null_aggressor_id, 9_500_996_i64)] {
                s.items.insert(sid, InventoryItemRecord {
                    item_id: sid, type_id: PORPOISE, owner_id: oid, location_id: i64::from(null_sys),
                    flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
                });
            }
        });
        for (sid, x) in [(p.ship_id, 0.0), (null_neutral_id, 5_000.0), (null_aggressor_id, 6_000.0)] {
            null_handle.spawn_ball(Ball {
                id: sid, type_id: PORPOISE, position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            }).await.unwrap();
        }
        null_handle.add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
            ship_id: null_aggressor_id, module_id: 9_500_902, module_type_id: 527, owner_char_id: Some(9_500_996),
            effect_name: "warpScrambleForEntity".into(), target_id: Some(p.ship_id), repeat: 1, duration_s: 10.0, ..Default::default()
        }).await.unwrap();
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
        let null_ats = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
        let null_locked_ids: Vec<i64> = dict_get(&null_ats, "autoLockingTargetIDs").and_then(|v| match v {
            PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect()),
            _ => None,
        }).unwrap_or_default();
        assert!(!null_locked_ids.contains(&null_neutral_id), "ATS must NOT auto-lock passive neutral player in null-sec");
        assert!(null_locked_ids.contains(&null_aggressor_id), "ATS must auto-lock active offensive aggressor on initial activation");
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
        p.ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.solar_system_id = JITA; } });

        // 2. ARSB consumes cap booster charge instead of capacitor while loaded, falls back to capacitor when unloaded, and reloads in 60s
        let target_ship_id = neutral_ship_id;
        lock(&mut p, target_ship_id).await;
        let _ = sys_handle.try_update_combat_state(p.ship_id, |c| {
            c.capacitor = 10_000.0;
            c.max_capacitor = 10_000.0;
        });
        let cap_before = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(10_000.0);
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arsb_id), PyRep::Integer(target_ship_id)]).await.unwrap();
        let cap_after_loaded = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(0.0);
        assert!((cap_before - cap_after_loaded).abs() < 1e-3, "ARSB must consume 0 capacitor while loaded");
        assert!(
            p.ctx.db.with_snapshot(|s| s.items.get(&cap_charge_id).is_none()),
            "ARSB must consume 1 cap booster charge on activation"
        );
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arsb_id)]).await;
        p.ctx.clock().advance_secs(60);
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arsb_id), PyRep::Integer(target_ship_id)]).await.unwrap();
        let cap_after_unloaded = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(0.0);
        assert!(cap_after_unloaded < cap_after_loaded - 1.0, "Unloaded ARSB must fall back to consuming capacitor");

        // 3. ARAR gives 3x repair bonus with Nanite Repair Paste vs unloaded fallback, and reloads in 60s
        let _ = sys_handle.try_update_combat_state(target_ship_id, |c| { c.armor_hp = 100.0; });
        let armor_0 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arar_id), PyRep::Integer(target_ship_id)]).await.unwrap();
        sys_handle.step_manual(12.0).await.unwrap();
        let armor_1 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
        let repaired_loaded = armor_1 - armor_0;
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arar_id)]).await;

        p.ctx.clock().advance_secs(60);
        p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&paste_id); });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let _ = sys_handle.try_update_combat_state(target_ship_id, |c| { c.armor_hp = 100.0; });
        let armor_2 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
        let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arar_id), PyRep::Integer(target_ship_id)]).await.unwrap();
        sys_handle.step_manual(12.0).await.unwrap();
        let armor_3 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
        let repaired_unloaded = armor_3 - armor_2;
        assert!(repaired_unloaded > 0.0, "Unloaded ARAR must fall back to base remote armor repair");
        assert!(
            repaired_loaded > repaired_unloaded * 2.5,
            "Loaded ARAR ({repaired_loaded}) must apply 3x paste repair bonus over unloaded ({repaired_unloaded})"
        );
        let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arar_id)]).await;

        let reload_cap_id = 9_500_008_i64;
        insert_item(&p.ctx, reload_cap_id, 11_283, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 4);
        let ship_id = p.ship_id;
        for (mod_id, ammo_id) in [(arar_id, reload_paste_id), (arsb_id, reload_cap_id)] {
            let load_rep = rpc(
                &mut p,
                "dogmaIM",
                None,
                "LoadAmmo",
                &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)])],
            )
            .await
            .unwrap();
            let reload_ms = dict_get(&load_rep, "reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
            assert!((reload_ms - 60_000.0).abs() < 1e-3, "Ancillary remote repairer {mod_id} reload must be 60s (60000 ms), got {reload_ms}");
        }

        // 3. Unloaded Mining Foreman Burst II with charges ONLY in Cargo must fail with NoChargesLoaded (no cargo bypass)
        let (cargo_burst_id, cargo_charge_id) = (9_500_009_i64, 9_500_010_i64);
        insert_item(&p.ctx, cargo_burst_id, 43_551, CHAR_ID, p.ship_id, InventoryFlag::HiSlot4, 1);
        insert_item(&p.ctx, cargo_charge_id, 42_830, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 10);
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let cargo_burst_err = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(cargo_burst_id)]).await.unwrap_err();
        assert!(cargo_burst_err.contains("NoChargesLoaded"), "Unloaded Command Burst must not consume charges from Cargo: {cargo_burst_err}");
        assert_eq!(
            p.ctx.db.with_snapshot(|s| s.items.get(&cargo_charge_id).map(|i| i.quantity)),
            Some(10),
            "Cargo burst charges must remain untouched when burst slot is unloaded"
        );
    }
}

pub(crate) mod auto_targeting_cargo_expanders {
    // End-to-End Integration Tests for Batch 34 `feat:modules.*` parity rows.

    use crate::modules_advanced_tactical::*;
    use eve_db::{FleetRecord, InventoryFlag};
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
    use std::collections::HashMap;

    #[tokio::test]
    async fn b34_auto_targeting_and_cargo_expanders() {
        let ctx = build_ctx();
        let mut sess = seed_pilot(&ctx, 94001, 84001, 621, NULL_SYS, true); // Caracal (621)
        let base_info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await
            .unwrap();
        let base_cargo = attr_from_all_info(&base_info, "shipInfo", 38);
        let base_vel = attr_from_all_info(&base_info, "shipInfo", 37);
        let base_hull = attr_from_all_info(&base_info, "shipInfo", 9);

        // Fit Auto Targeting System I (1182), Expanded Cargohold I (1317), and Medium Cargohold Optimization I (31119)
        fit_mod(&ctx, &mut sess, 84001, 74001, 1182, 27).await.expect("fit Auto Targeting System I");
        fit_mod(&ctx, &mut sess, 84001, 74002, 1317, 11).await.expect("fit Expanded Cargohold I");
        fit_mod(&ctx, &mut sess, 84001, 74003, 31119, 92).await.expect("fit Medium Cargohold Optimization I");

        let mod_info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await
            .unwrap();
        assert!(attr_from_all_info(&mod_info, "shipInfo", 38) > base_cargo * 1.35, "Expanded Cargohold + rig must increase cargo capacity");
        assert!(attr_from_all_info(&mod_info, "shipInfo", 37) < base_vel, "Cargo expander/rig must reduce maxVelocity");
        assert!(attr_from_all_info(&mod_info, "shipInfo", 9) < base_hull, "Expanded Cargohold must reduce hull HP");

        sess.station_id = None;
        spawn_in_space(&ctx, 94001, 84001, 621, NULL_SYS, DVec3::ZERO).await;
        let _hostile = seed_pilot(&ctx, 94002, 84002, 587, NULL_SYS, false);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(c) = s.characters.get_mut(&94002) {
                c.alliance_id = Some(99000099);
            }
        });
        spawn_in_space(&ctx, 94002, 84002, 587, NULL_SYS, DVec3::new(15_000.0, 0.0, 0.0)).await;
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.add_limited_engagement(94001, 94002, ctx.now_unix() + 900);
        });

        let act = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74001), PyRep::String("default".into())])
            .await
            .expect("activate Auto Targeting System I");
        assert_eq!(act.dict_get("active").and_then(|v| v.as_bool()), Some(true));
        assert!(act.dict_get("maxLockedTargets").and_then(|v| v.as_i64()).unwrap_or(0) >= 8);
        assert_eq!(act.dict_get("lockingCount").and_then(|v| v.as_i64()), Some(1));
    }

    #[tokio::test]
    async fn b34_bastion_effects_and_wormhole_mjd_interaction() {
        let ctx = build_ctx();
        let mut sess = seed_pilot(&ctx, 94010, 84010, 28659, NULL_SYS, true); // Paladin (28659, Marauder)
        fit_mod(&ctx, &mut sess, 84010, 74011, 33400, 27).await.expect("fit Bastion Module I");
        fit_mod(&ctx, &mut sess, 84010, 74012, 4383, 19).await.expect("fit Large MJD");
        sess.station_id = None;
        spawn_in_space(&ctx, 94010, 84010, 28659, NULL_SYS, DVec3::ZERO).await;

        let act = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74011), PyRep::String("default".into())])
            .await
            .expect("activate Bastion");
        assert_eq!(act.dict_get("active").and_then(|v| v.as_bool()), Some(true));

        let info = rpc(&ctx, &mut sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await
            .unwrap();
        assert_eq!(attr_from_all_info(&info, "shipInfo", 2112), 1.0, "Bastion must NOT grant ECM immunity");
        assert!((attr_from_all_info(&info, "shipInfo", 2135) - 0.05).abs() < 1e-6, "Bastion remoteAssistanceImpedance = 0.05");
        assert_eq!(attr_from_all_info(&info, "shipInfo", 2045), 0.0, "Bastion blocks incoming remote repair");

        // MJD activation is refused while Bastion is active
        let mjd_err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(74012), PyRep::String("default".into())])
            .await
            .unwrap_err();
        assert_eq!(mjd_err.code, "WarpScrambled");

        // Command Destroyer MJFG still moves the bastioned Marauder
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let mut cd_sess = seed_pilot(&ctx, 94011, 84011, 37483, NULL_SYS, true); // Pontifex (37483)
        fit_mod(&ctx, &mut cd_sess, 84011, 74013, 37479, 19).await.expect("fit MJFG");
        cd_sess.station_id = None;
        spawn_in_space(&ctx, 94011, 84011, 37483, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let moved = sys.micro_jump_field(84011, 100_000.0, 6_000.0, 25).await.expect("MJFG boosh");
        assert!(moved.contains(&84010), "MJFG must move bastioned Marauder");

        // Bastioned Marauder cannot jump a wormhole (even from 10 km away) and Bastion stays active
        let wh_err = rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", vec![PyRep::Integer(750_000_001)])
            .await
            .unwrap_err();
        assert_eq!(wh_err.code, "CannotJumpWormholeWhileImmobilized");
        let still_active = ctx.db.with_snapshot(|s| s.module_active_states.contains(&74011));
        assert!(still_active, "Bastion module must remain active when WormholeJump is rejected");
    }

    #[tokio::test]
    async fn b34_cehe_and_capital_mjd_mjfg() {
        let ctx = build_ctx();
        // Subcapital cannot activate CEHE
        let mut sub_sess = seed_pilot(&ctx, 94020, 84020, 621, NULL_SYS, true);
        give_item(&ctx, 74021, 40714, 94020, 84020, InventoryFlag::LoSlot0, 1);
        sub_sess.station_id = None;
        spawn_in_space(&ctx, 94020, 84020, 621, NULL_SYS, DVec3::ZERO).await;
        assert_eq!(
            rpc(&ctx, &mut sub_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74021), PyRep::String("default".into())])
                .await
                .unwrap_err()
                .code,
            "InvalidShipGroup"
        );

        // Thanatos (23911, Carrier) activates CEHE (40714) and Capital MJD (83465)
        let mut rev_sess = seed_pilot(&ctx, 94021, 84021, 23911, NULL_SYS, true);
        fit_mod(&ctx, &mut rev_sess, 84021, 74022, 40714, 11).await.expect("fit CEHE I");
        fit_mod(&ctx, &mut rev_sess, 84021, 74023, 83465, 19).await.expect("fit Capital MJD");
        rev_sess.station_id = None;
        spawn_in_space(&ctx, 94021, 84021, 23911, NULL_SYS, DVec3::ZERO).await;

        let cehe_act = rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74022), PyRep::String("default".into())])
            .await
            .expect("activate CEHE I");
        assert_eq!(cehe_act.dict_get("active").and_then(|v| v.as_bool()), Some(true));
        let info = rpc(&ctx, &mut rev_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await
            .unwrap();
        assert!(attr_from_all_info(&info, "shipInfo", 113) <= 0.05 + 1e-6, "CEHE must grant massive hull EM resistance");

        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        // Activate Capital MJD (83465) via dogmaIM.Activate and advance cycle ticks (B34-F-01)
        rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74023), PyRep::String("default".into())])
            .await
            .expect("activate Capital MJD");
        let _ = sys.step_manual(180.0).await;
        let cap_pos = sys.get_ball_snapshot(84021).await.unwrap().unwrap().1.position;
        assert!((cap_pos.length() - 250_000.0).abs() < 100.0, "Capital MJD must jump 250 km forward via cycle, got {cap_pos:?}");

        // Capital MJFG (83485) on Thanatos (23911, Carrier) jumps subcapitals within 10 km 250 km forward via cycle
        if let Ok(Some(mut cs)) = sys.get_combat_state(84020).await {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.gate_cloak_remaining_ticks = 0;
            let _ = sys.set_combat_state(84020, cs).await;
        }
        let mut car_sess = seed_pilot(&ctx, 94022, 84022, 23911, NULL_SYS, true);
        fit_mod(&ctx, &mut car_sess, 84022, 74024, 83485, 19).await.expect("fit Capital MJFG");
        car_sess.station_id = None;
        spawn_in_space(&ctx, 94022, 84022, 23911, NULL_SYS, DVec3::ZERO).await;
        rpc(&ctx, &mut car_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74024), PyRep::String("default".into())])
            .await
            .expect("activate Capital MJFG");
        let _ = sys.step_manual(180.0).await;
        let mjfg_pos = sys.get_ball_snapshot(84022).await.unwrap().unwrap().1.position;
        let sub_pos = sys.get_ball_snapshot(84020).await.unwrap().unwrap().1.position;
        let cap_other_pos = sys.get_ball_snapshot(84021).await.unwrap().unwrap().1.position;
        assert!((mjfg_pos.length() - 250_000.0).abs() < 100.0, "Capital MJFG caster must jump 250 km forward");
        assert!(sub_pos.x > 240_000.0, "Capital MJFG must jump subcapital within 10 km, got sub_pos={sub_pos:?}");
        assert!((cap_other_pos.length() - 250_000.0).abs() < 100.0, "Capital MJFG must NOT jump other capital ships");
    }

    #[tokio::test]
    async fn b34_cloak_stabilization_and_clone_vat_bay_fuel() {
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let mut cov_sess = seed_pilot(&ctx, 94030, 84030, 11957, NULL_SYS, true);
        fit_mod(&ctx, &mut cov_sess, 84030, 74030, 11578, 27).await.expect("fit Covert Ops Cloaking Device II");
        cov_sess.station_id = None;
        spawn_in_space(&ctx, 94030, 84030, 11957, NULL_SYS, DVec3::new(5_000_000_000.0, 0.0, 0.0)).await;

        // Anchor a real Mobile Observatory (10m deploy + 10m ping interval) > 2km away to avoid proximity decloak
        let mut obs_sess = seed_pilot(&ctx, 94039, 84039, 587, NULL_SYS, true);
        let obs_id = 74039_i64;
        ctx.db.with_mut_snapshot(|s| {
            s.skills.entry(94039).or_default().insert(
                3392,
                eve_db::TrainedSkillRecord {
                    type_id: 3392,
                    level: 3,
                    skill_points: 10_000.0,
                },
            );
        });
        give_item(
            &ctx,
            obs_id,
            eve_server::services::mobile_deployables::TYPE_MOBILE_OBSERVATORY,
            94039,
            84039,
            InventoryFlag::Cargo,
            1,
        );
        obs_sess.station_id = None;
        spawn_in_space(&ctx, 94039, 84039, 587, NULL_SYS, DVec3::new(5_100_000_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut obs_sess, "posMgr", "AnchorStructure", vec![PyRep::Integer(obs_id)])
            .await
            .expect("anchor Mobile Observatory");

        // Activate cloak through dogmaIM.Activate RPC and assert cloak_stabilization_remaining_s (B34-F-02)
        rpc(&ctx, &mut cov_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
            .await
            .expect("activate Covert Ops Cloak");
        let cs = sys.get_combat_state(84030).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "Ship must be cloaked after Activate");
        assert!((cs.cloak_stabilization_remaining_s - 900.0).abs() < 1.0, "Cloak stabilization timer must start at 900s");

        // Advance 601s so the Mobile Observatory finishes 10m deploy and fires its first decloak ping;
        // ship has only been cloaked ~601s (< 900s stabilization timer) so it must remain cloaked
        ctx.advance_virtual_clock(601);
        for _ in 0..10 {
            sys.step_manual(60.1).await.unwrap();
        }
        let cs_mid = sys.get_combat_state(84030).await.unwrap().unwrap();
        assert!(cs_mid.is_cloaked, "Ship must be immune to observatory ping while stabilization timer > 0");
        assert!(cs_mid.cloak_stabilization_remaining_s > 0.0, "Stabilization timer must still be positive at 601s");

        // Deactivating cloak resets cloak_stabilization_remaining_s to 0.0
        rpc(&ctx, &mut cov_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
            .await
            .expect("deactivate Covert Ops Cloak");
        let cs_decloaked = sys.get_combat_state(84030).await.unwrap().unwrap();
        assert!(!cs_decloaked.is_cloaked, "Ship must be decloaked");
        assert_eq!(cs_decloaked.cloak_stabilization_remaining_s, 0.0, "Decloak must reset stabilization timer to 0");

        // Re-cloak and advance virtual clock + simulation past 900s -> real observatory ping decloaks ship
        ctx.advance_virtual_clock(35);
        rpc(&ctx, &mut cov_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74030), PyRep::String("default".into())])
            .await
            .expect("re-activate Covert Ops Cloak");
        ctx.mechanic_rng.set_seed(Some(42));
        ctx.advance_virtual_clock(3_000);
        for _ in 0..50 {
            sys.step_manual(60.0).await.unwrap();
        }
        let cs_after = sys.get_combat_state(84030).await.unwrap().unwrap();
        assert!(!cs_after.is_cloaked, "Expired stabilization timer (>900s) must allow real observatory ping to decloak");

        // Clone Vat Bay I (23735) on Rorqual (28352) requires Liquid Ozone, immobilizes ship, and gates clone install on active cycle
        let mut rorq_sess = seed_pilot(&ctx, 94031, 84031, 28352, NULL_SYS, true);
        let mut guest_sess = seed_pilot(&ctx, 94032, 84032, 621, NULL_SYS, false);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(5001, FleetRecord {
                fleet_id: 5001,
                boss_id: 94031,
                free_move: true,
                members: vec![94031, 94032],
                ..Default::default()
            });
        });
        rorq_sess.fleet_id = Some(5001);
        guest_sess.fleet_id = Some(5001);
        fit_mod(&ctx, &mut rorq_sess, 84031, 74031, 23735, 27).await.expect("fit Clone Vat Bay I");
        rorq_sess.station_id = None;
        spawn_in_space(&ctx, 94031, 84031, 28352, NULL_SYS, DVec3::ZERO).await;

        // Fails without Liquid Ozone
        assert_eq!(
            rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74031), PyRep::String("default".into())])
                .await
                .unwrap_err()
                .code,
            "NotEnoughFuel"
        );
        give_item(&ctx, 74032, 16273, 94031, 84031, InventoryFlag::Cargo, 600);
        let cvb_act = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74031), PyRep::String("default".into())])
            .await
            .expect("activate Clone Vat Bay I");
        assert_eq!(cvb_act.dict_get("cloneVatBayActive").and_then(|v| v.as_bool()), Some(true));
        assert!(cvb_act.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        // Offering clone succeeds while active
        rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(94032), PyRep::Integer(84031)])
            .await
            .expect("offer clone while active");
        // Deactivating Clone Vat Bay blocks subsequent clone installation
        ctx.db.with_mut_snapshot(|s| {
            s.module_active_states.remove(&74031);
        });
        assert_eq!(
            rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(94032), PyRep::Integer(84031)])
                .await
                .unwrap_err()
                .code,
            "NoActiveCloneVatBay"
        );
    }

    #[tokio::test]
    async fn b34_command_bursts_strength_range_duration_fitting() {
        let ctx = build_ctx();
        // Alpha clone cannot activate Command Burst
        let mut alpha_sess = seed_pilot(&ctx, 94040, 84040, 22474, NULL_SYS, true); // Damnation (22474, Command Ship)
        fit_mod(&ctx, &mut alpha_sess, 84040, 74041, 42526, 27).await.expect("fit Armor Command Burst I");
        give_item(&ctx, 74042, 42832, 94040, 84040, InventoryFlag::HiSlot0, 10);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(c) = s.characters.get_mut(&94040) {
                c.clone_grade = 0;
            }
        });
        alpha_sess.station_id = None;
        spawn_in_space(&ctx, 94040, 84040, 22474, NULL_SYS, DVec3::ZERO).await;
        assert_eq!(
            rpc(&ctx, &mut alpha_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74041), PyRep::String("default".into())])
                .await
                .unwrap_err()
                .code,
            "OmegaRequiredForCommandBurst"
        );

        // Omega Damnation + Command Processor I rig (11014) + Mindlink + Alpha fleet member in range
        let mut cmd_sess = seed_pilot(&ctx, 94041, 84041, 22474, NULL_SYS, true);
        let mut mem_sess = seed_pilot(&ctx, 94042, 84042, 621, NULL_SYS, false);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(c) = s.characters.get_mut(&94042) {
                c.clone_grade = 0; // Alpha fleet member still receives effect
            }
            s.fleets.insert(5002, FleetRecord {
                fleet_id: 5002,
                boss_id: 94041,
                free_move: true,
                members: vec![94041, 94042],
                ..Default::default()
            });
        });
        cmd_sess.fleet_id = Some(5002);
        mem_sess.fleet_id = Some(5002);
        fit_mod(&ctx, &mut cmd_sess, 84041, 74043, 43552, 27).await.expect("fit Armor Command Burst II");
        give_item(&ctx, 74044, 42832, 94041, 84041, InventoryFlag::HiSlot0, 10);
        fit_mod(&ctx, &mut cmd_sess, 84041, 74045, 11014, 19).await.expect("fit Command Processor I");
        cmd_sess.station_id = None;
        spawn_in_space(&ctx, 94041, 84041, 22474, NULL_SYS, DVec3::ZERO).await;
        spawn_in_space(&ctx, 94042, 84042, 621, NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;

        let burst = rpc(&ctx, &mut cmd_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74043), PyRep::String("default".into())])
            .await
            .expect("activate Command Burst");
        let aoe_range = burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0);
        // 15km * 2.0 (Command Ship) * (1+0.50 Wing Cmd) * (1+0.25 Fleet Cmd) = 56,250.0 m
        assert!((aoe_range - 56_250.0).abs() < 1.0, "Expected Command Ship burst range 56250.0m, got {aoe_range}");
        assert_eq!(burst.dict_get("reloadTimeSeconds").and_then(|v| v.as_f64()), Some(30.0));
        assert!(burst.dict_get("buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 90.0);
        assert!(burst.dict_get("buffStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > 2.0);
    }

    #[tokio::test]
    async fn b34_cyno_covert_cyno_portal_jammer_and_lance() {
        let ctx = build_ctx();
        let dest_sys = 30004760u32;
        let mut cyno_sess = seed_pilot(&ctx, 94050, 84050, 11957, dest_sys, true); // Falcon (11957)
        let mut blops_sess = seed_pilot(&ctx, 94051, 84051, 22428, NULL_SYS, true); // Redeemer (22428)
        let mut cov_sess = seed_pilot(&ctx, 94052, 84052, 12032, NULL_SYS, false); // Manticore (12032, Stealth Bomber)
        let mut non_cov_sess = seed_pilot(&ctx, 94053, 84053, 621, NULL_SYS, false); // Caracal (621, non-covert)

        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(5003, FleetRecord {
                fleet_id: 5003,
                boss_id: 94051,
                free_move: true,
                members: vec![94050, 94051, 94052, 94053],
                ..Default::default()
            });
        });
        for s in [&mut cyno_sess, &mut blops_sess, &mut cov_sess, &mut non_cov_sess] {
            s.fleet_id = Some(5003);
        }

        // Tenebrex Cyno Jammer spool-up (300s) and reinforcement disable
        let now_u = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|s| {
            s.structures.insert(998_101, eve_db::UpwellStructureRecord {
                structure_id: 998_101,
                type_id: 32458,
                owner_corp_id: 1000125,
                solar_system_id: dest_sys,
                name: "Sov Hub".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
            s.sov_runtime.sov_hubs.insert(
                998_101,
                eve_db::PersistedSovHubData {
                    hub_id: 998_101,
                    upgrades: vec![eve_db::PersistedHubUpgrade {
                        type_id: eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION,
                        online: true,
                        low_power: false,
                    }],
                    ..Default::default()
                },
            );
            s.structures.insert(998_100, eve_db::UpwellStructureRecord {
                structure_id: 998_100,
                type_id: 37534,
                owner_corp_id: 1000125,
                solar_system_id: dest_sys,
                name: "Tenebrex".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
            s.sov_runtime.tenebrex_spool_ready_unix.insert(998_100, now_u + 300);
        });
        assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, dest_sys).is_none(), "Tenebrex must spool up for 300s");
        ctx.advance_virtual_clock(305);
        assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, dest_sys).is_some(), "Tenebrex must jam after 300s");

        // Covert cyno (28646) can be lit in jammed system, uses 25 LO, not visible on map
        fit_mod(&ctx, &mut cyno_sess, 84050, 74051, 28646, 27).await.expect("fit Covert Cyno");
        give_item(&ctx, 74052, 16273, 94050, 84050, InventoryFlag::Cargo, 100);
        cyno_sess.station_id = None;
        spawn_in_space(&ctx, 94050, 84050, 11957, dest_sys, DVec3::ZERO).await;
        let cov_cyno = rpc(&ctx, &mut cyno_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74051), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("light covert cyno in jammed system");
        let beacon_id = cov_cyno.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(cov_cyno.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(5));
        let map_counts = rpc(&ctx, &mut cyno_sess, "map", "GetBeaconCount", vec![]).await.unwrap();
        if let PyRep::Dict(entries) = map_counts {
            assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(dest_sys as i64)), "Covert cyno must NOT appear on map");
        }

        // Covert Jump Portal Generator (28652) on Redeemer bridges covert hull and rejects non-covert hull
        fit_mod(&ctx, &mut blops_sess, 84051, 74053, 28652, 27).await.expect("fit Covert Jump Portal");
        give_item(&ctx, 74054, 16273, 94051, 84051, InventoryFlag::Cargo, 1_000);
        give_item(&ctx, 74055, 16274, 94051, 84051, InventoryFlag::Cargo, 50_000); // Helium Isotopes
        blops_sess.station_id = None;
        spawn_in_space(&ctx, 94051, 84051, 22428, NULL_SYS, DVec3::ZERO).await;
        spawn_in_space(&ctx, 94052, 84052, 12032, NULL_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
        spawn_in_space(&ctx, 94053, 84053, 621, NULL_SYS, DVec3::new(500.0, 0.0, 0.0)).await;

        let b_id = ctx.bind_object(&mut blops_sess, "beyonce", NULL_SYS as i64).unwrap();
        ctx.dispatch_rpc(&mut blops_sess, "beyonce", Some(b_id), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(94050), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None)
            .await
            .expect("open covert bridge");

        let non_cov_b = ctx.bind_object(&mut non_cov_sess, "beyonce", NULL_SYS as i64).unwrap();
        let rej = ctx.dispatch_rpc(&mut non_cov_sess, "beyonce", Some(non_cov_b), "CmdJumpThroughFleet", &[PyRep::Integer(94051), PyRep::Integer(84051)], &HashMap::new(), None)
            .await
            .unwrap_err();
        assert_eq!(rej.code, "JumpBridgeRestrictedShip");

        let cov_b = ctx.bind_object(&mut cov_sess, "beyonce", NULL_SYS as i64).unwrap();
        ctx.dispatch_rpc(&mut cov_sess, "beyonce", Some(cov_b), "CmdJumpThroughFleet", &[PyRep::Integer(94051), PyRep::Integer(84051)], &HashMap::new(), None)
            .await
            .expect("covert ship jumps through covert portal");

        // Disruptive Lance (77398) on Valravn (77288, Lancer Dreadnought)
        let mut bane_sess = seed_pilot(&ctx, 94054, 84054, 77288, NULL_SYS, true);
        fit_mod(&ctx, &mut bane_sess, 84054, 74056, 77398, 27).await.expect("fit Disruptive Lance");
        fit_mod(&ctx, &mut bane_sess, 84054, 74057, 20280, 28).await.expect("fit Siege Module I");
        give_item(&ctx, 74058, 16275, 94054, 84054, InventoryFlag::Cargo, 500);
        give_item(&ctx, 74059, 17889, 94054, 84054, InventoryFlag::Cargo, 50_000);
        bane_sess.station_id = None;
        spawn_in_space(&ctx, 94054, 84054, 77288, NULL_SYS, DVec3::ZERO).await;
        rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74057), PyRep::String("default".into())])
            .await
            .expect("activate Siege");
        let lance = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74056), PyRep::String("default".into())])
            .await
            .expect("activate Disruptive Lance");
        assert!((lance.dict_get("totalDamageApplied").and_then(|v| v.as_f64()).unwrap_or(0.0) - 382_500.0).abs() < 1.0);
        assert_eq!(lance.dict_get("neutralizerAmountGJ").and_then(|v| v.as_f64()), Some(31_000.0));
    }
}

pub(crate) mod normal_cyno_lighting_rules {
    // Batch 34 Part 2: Normal Cyno Lighting Rules, Tenebrex Jammer State/Reinforcement,
    // and Strengthened Bastion & Command Burst Assertions.

    use crate::modules_advanced_tactical::*;
    use eve_db::{FleetRecord, InventoryFlag, UpwellStructureRecord};
    use eve_destiny::{Ball, DVec3, MovementMode};
    use eve_proto::PyRep;
    use std::collections::HashMap;

    #[tokio::test]
    async fn b34_normal_cyno_lighting_rules() {
        let ctx = build_ctx();

        // 1. Not in fleet (when another fleet exists on server) -> MustBeInFleet
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(5100, FleetRecord {
                fleet_id: 5100,
                boss_id: 94199,
                free_move: true,
                members: vec![94199],
                ..Default::default()
            });
        });
        let mut solo_sess = seed_pilot(&ctx, 94101, 84101, 11957, NULL_SYS, true);
        fit_mod(&ctx, &mut solo_sess, 84101, 74101, 21096, 27).await.expect("fit Cynosural Field Generator I");
        give_item(&ctx, 74102, 16273, 94101, 84101, InventoryFlag::Cargo, 1_000);
        solo_sess.station_id = None;
        spawn_in_space(&ctx, 94101, 84101, 11957, NULL_SYS, DVec3::ZERO).await;

        let err_no_fleet = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
            .await
            .unwrap_err();
        assert_eq!(err_no_fleet.code, "MustBeInFleet", "Normal cyno must require fleet when fleets exist");

        // 2. High-sec system (30000142 Jita) -> CannotLightCynoInSystem
        let mut hs_sess = seed_pilot(&ctx, 94102, 84102, 11957, 30_000_142, true);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(5101, FleetRecord {
                fleet_id: 5101,
                boss_id: 94102,
                free_move: true,
                members: vec![94102, 94101],
                ..Default::default()
            });
        });
        hs_sess.fleet_id = Some(5101);
        fit_mod(&ctx, &mut hs_sess, 84102, 74103, 21096, 27).await.expect("fit cyno in highsec");
        give_item(&ctx, 74104, 16273, 94102, 84102, InventoryFlag::Cargo, 1_000);
        hs_sess.station_id = None;
        spawn_in_space(&ctx, 94102, 84102, 11957, 30_000_142, DVec3::ZERO).await;
        let err_hs = rpc(&ctx, &mut hs_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74103), PyRep::String("cynosuralGeneration".into())])
            .await
            .unwrap_err();
        assert_eq!(err_hs.code, "CannotLightCynoInSystem");

        // 3. Speed >= 500 m/s -> ShipMovingTooFast
        solo_sess.fleet_id = Some(5101);
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let _ = sys.spawn_ball(Ball {
            id: 84101,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::new(600.0, 0.0, 0.0),
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.6,
            mode: MovementMode::Stop,
        }).await;
        let err_fast = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
            .await
            .unwrap_err();
        assert_eq!(err_fast.code, "ShipMovingTooFast");

        // Stop ship
        let _ = sys.spawn_ball(Ball {
            id: 84101,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;

        // 4. Insufficient Liquid Ozone (< 50 LO on Force Recon) -> NotEnoughFuel
        ctx.db.with_mut_snapshot(|s| {
            if let Some(lo) = s.items.get_mut(&74102) {
                lo.quantity = 40;
            }
        });
        let err_lo = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
            .await
            .unwrap_err();
        assert_eq!(err_lo.code, "NotEnoughFuel");

        // 5. Valid normal cyno activation: consumes 50 LO (Force Recon 90% reduction from 500 LO), 5-minute (300s) cycle
        ctx.db.with_mut_snapshot(|s| {
            if let Some(lo) = s.items.get_mut(&74102) {
                lo.quantity = 100;
            }
        });
        let ok_cyno = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("light normal cyno");
        assert_eq!(ok_cyno.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(50));
        let rem_lo = ctx.db.with_snapshot(|s| s.items.get(&74102).map(|i| i.quantity).unwrap_or(0));
        assert_eq!(rem_lo, 50, "Normal cyno must consume 50 Liquid Ozone on Force Recon");
    }

    #[tokio::test]
    async fn b34_tenebrex_jammer_state_and_reinforcement() {
        let ctx = build_ctx();
        let jam_sys = 30_004_761u32;
        let mut sess = seed_pilot(&ctx, 94110, 84110, 621, jam_sys, true);
        sess.station_id = None;
        spawn_in_space(&ctx, 94110, 84110, 621, jam_sys, DVec3::ZERO).await;

        let now_u = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|s| {
            s.structures.insert(998_200, UpwellStructureRecord {
                structure_id: 998_200,
                type_id: 37534,
                owner_corp_id: 1000125,
                solar_system_id: jam_sys,
                name: "Tenebrex".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
            s.sov_runtime.tenebrex_spool_ready_unix.insert(998_200, now_u + 300);
        });

        // Hubless Tenebrex must NOT jam even after spool
        ctx.advance_virtual_clock(305);
        assert!(
            eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none(),
            "Hubless Tenebrex must NOT jam cynos"
        );

        // Seed Sovereignty Hub with Cynosural Suppression upgrade + fresh 300s spool
        let now_u2 = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|s| {
            s.structures.insert(998_201, UpwellStructureRecord {
                structure_id: 998_201,
                type_id: 32458,
                owner_corp_id: 1000125,
                solar_system_id: jam_sys,
                name: "Sov Hub".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
            s.sov_runtime.sov_hubs.insert(
                998_201,
                eve_db::PersistedSovHubData {
                    hub_id: 998_201,
                    upgrades: vec![eve_db::PersistedHubUpgrade {
                        type_id: eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION,
                        online: true,
                        low_power: false,
                    }],
                    ..Default::default()
                },
            );
            s.sov_runtime.tenebrex_spool_ready_unix.insert(998_200, now_u2 + 300);
        });

        // Spooling up -> not active yet
        assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none());

        // After 300s spool -> active
        ctx.advance_virtual_clock(305);
        assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_some());

        let b_id = ctx.bind_object(&mut sess, "beyonce", jam_sys as i64).unwrap();
        let state_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
            .await
            .expect("GetCynoJammerState");
        assert!(matches!(state_rep, PyRep::Integer(ts) if ts > 0), "GetCynoJammerState must return online timestamp when jammed, got {state_rep:?}");

        // Armor/hull reinforcement disables Tenebrex cyno jammer
        ctx.db.with_mut_snapshot(|s| {
            if let Some(st) = s.structures.get_mut(&998_200) {
                st.state = "ArmorReinforced".into();
                st.reinforced_until_unix = ctx.now_unix_secs() + 86_400;
            }
        });
        assert!(
            eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none(),
            "Reinforced Tenebrex must NOT jam cynos"
        );
        let state_after = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
            .await
            .expect("GetCynoJammerState after reinforcement");
        assert_eq!(state_after, PyRep::None, "GetCynoJammerState must return None when reinforced");
    }
}
