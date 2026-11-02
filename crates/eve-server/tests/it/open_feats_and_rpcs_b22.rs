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

    // 1. Create ItemExchange contract with BPO, BPC, and abyssal module
    let create_ie = rpc(
        &ctx,
        &mut sess1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::String("ItemExchange".into()),
            PyRep::String("Public".into()),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(7),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(JITA_STATION),
            PyRep::Real(1_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("BPO + BPC + Abyssal".into()),
            PyRep::String("".into()),
            PyRep::List(vec![
                PyRep::Integer(bpo_id),
                PyRep::Integer(bpc_id),
                PyRep::Integer(abyssal_id),
            ]),
        ],
    )
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

    let search = rpc(&ctx, &mut sess2, "contractProxy", "SearchContracts", &[PyRep::String("ItemExchange".into())])
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

    // Accept contract and verify singleton metadata preserved on transferred items
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(ie_cid)])
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

    // 2. Courier plastic wrap break forfeits collateral
    let create_courier = rpc(
        &ctx,
        &mut sess1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::String("Courier".into()),
            PyRep::String("Public".into()),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(3),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(JITA_STATION),
            PyRep::Real(0.0),
            PyRep::Real(250_000.0),
            PyRep::Real(2_000_000.0),
            PyRep::String("Courier Wrap Test".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(courier_cargo_id)]),
        ],
    )
    .await
    .expect("CreateContract Courier");
    let courier_cid = field(&create_courier, "contractID").as_i64().unwrap_or_else(|| create_courier.as_i64().unwrap());
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(courier_cid)])
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

    // Repackage (break) the plastic wrap
    rpc(
        &ctx,
        &mut sess2,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(wrap_id)]), PyRep::Bool(true)],
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

    // 1. Welcome mail setting + automatic delivery on join
    let deny_wm = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[PyRep::String("Hi".into()), PyRep::String("Body".into()), PyRep::Bool(true)],
    )
    .await;
    assert!(deny_wm.is_err(), "Non-member cannot set corp welcome mail");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[
            PyRep::String("Welcome to Vanguard".into()),
            PyRep::String("Read the corp bulletin!".into()),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("SetCorpWelcomeMail");

    let wm = rpc(&ctx, &mut sess1, "corpRegistry", "GetCorpWelcomeMail", &[])
        .await
        .expect("GetCorpWelcomeMail");
    assert_eq!(field(&wm, "subject"), PyRep::String("Welcome to Vanguard".into()));

    let app_rep = rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining!".into())])
        .await
        .expect("InsertApplication");
    let app_id = field(&app_rep, "applicationID").as_i64().unwrap_or_else(|| app_rep.as_i64().unwrap());

    rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into()), PyRep::Integer(6)])
        .await
        .expect("UpdateApplicationOffer");

    ctx.db.with_snapshot(|s| {
        assert!(
            s.mails.iter().any(|m| m.recipient_id == PILOT_2 && m.subject == "Welcome to Vanguard"),
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
        &[PyRep::Integer(st_id), PyRep::Integer(target_corp_id)],
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
        &[PyRep::Integer(st_id), PyRep::Integer(target_corp_id)],
    )
    .await
    .expect("Structure transfer must succeed when target corp accepts structures");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&st_id].owner_corp_id, target_corp_id);
    });
}
