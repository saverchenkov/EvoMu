//! End-to-end integration tests for Batch 22 open `feat:contracts.*` and `feat:corporation.*` rows.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;

async fn select_pilot(
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

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
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

fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
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
    assert!(found_contracts.iter().any(|c| field(c, "contractID") == PyRep::Integer(ie_cid)));

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
        "mailSvc",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(corp_id)]),
            PyRep::String("Corp Spam".into()),
            PyRep::String("Hello everyone".into()),
        ],
    )
    .await;
    assert!(restricted_send.is_err(), "Non-authorised member must be blocked from sending corp-wide mail");

    // 3. Accept structures setting
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

    // 4. Structure reinforce default adopted by newly deployed structure
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

    rpc(&ctx, &mut sess1, "structureDocking", "Undock", &[])
        .await
        .expect("Undock");
    let sys_id = sess1.solar_system_id.unwrap();
    let ship_id = sess1.ship_id.unwrap();
    let base_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(glam::DVec3::ZERO);
    ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, base_pos + glam::DVec3::new(0.0, 5.0e9, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: 35832, owner_id: PILOT_1, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
        });
    });
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
}

#[tokio::test]
async fn test_b22_corp_governance_shares_dividends_ceo_divisions_autokick() {
    // parity: feat:corporation.ceo-resign, feat:corporation.corp-division-names, feat:corporation.dividends, feat:corporation.pending-auto-kick, feat:corporation.share-transfer
    eve_testkit::attest(&[
        "feat:corporation.ceo-resign",
        "feat:corporation.corp-division-names",
        "feat:corporation.dividends",
        "feat:corporation.pending-auto-kick",
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

    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
    });

    // 1. Division names
    let deny_div = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(1), PyRep::String("Hack".into()), PyRep::String("HackW".into())],
    )
    .await;
    assert!(deny_div.is_err(), "Non-director cannot rename divisions");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(1), PyRep::String("Armory".into()), PyRep::String("Treasury".into())],
    )
    .await
    .expect("UpdateDivisionNames");
    let divs = rpc(&ctx, &mut sess1, "corpRegistry", "GetDivisionNames", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetDivisionNames");
    assert_eq!(field(&divs, "division1"), PyRep::String("Armory".into()));
    assert_eq!(field(&divs, "walletDivision1"), PyRep::String("Treasury".into()));

    // 2. Share transfers & shareholder queries
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(400)],
    )
    .await
    .expect("MoveCompanyShares");
    let sh_rep = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetSharesByShareholder");
    assert!(!rowset_rows(&sh_rep).is_empty());

    // 3. PayoutDividend (per-share)
    rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)])
        .await
        .expect("PayoutDividend");
    ctx.db.with_snapshot(|s| {
        assert!(s.characters[&PILOT_2].balance.raw_cents() > 0, "Shareholder must receive dividend");
    });

    // 4. Pending auto-kick when member is in space
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

    // 5. CEO resignation
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
    // parity: feat:corporation.corp-project-close-expiry, feat:corporation.corp-project-contribution-methods, feat:corporation.corp-project-reward-pool, feat:corporation.npc-corp-placement-rules, feat:corporation.npc-corp-tax-rate
    eve_testkit::attest(&[
        "feat:corporation.corp-project-close-expiry",
        "feat:corporation.corp-project-contribution-methods",
        "feat:corporation.corp-project-reward-pool",
        "feat:corporation.npc-corp-placement-rules",
        "feat:corporation.npc-corp-tax-rate",
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

    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
    });

    // 1. Corp project reward pool + contribution methods + close/refund
    let create_proj = rpc(
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
    .expect("CreateGoalSubmit");
    let pid = field(&create_proj, "projectID").as_i64().unwrap();

    // Server-observed mining harvest advances project & pays reward
    eve_server::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
        &ctx,
        PILOT_2,
        &["MineMaterials"],
        10,
        None,
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_projects[&pid].current_quantity, 10);
        assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 100_000_00);
    });

    // Close project and verify remaining escrow (900,000 ISK) refunded
    let bal_before_close = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents());
    rpc(&ctx, &mut sess1, "corporationSvc", "CloseCorpProject", &[PyRep::Integer(pid)])
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
    assert!((small_net - 67_500.0).abs() < 1e-3, "Bounty under 100k ISK must be tax-exempt");

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
    });
    let (large_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
    assert!((large_net - 1_201_500.0).abs() < 1e-3, "11% NPC corp tax must apply above 100k ISK threshold");
    ctx.db.with_snapshot(|s| {
        assert!(
            s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "bounty_prize_corporation_tax" && j.amount.raw_cents() == -148_500_00),
            "Wallet journal must record 11% NPC corp tax deduction"
        );
    });
}
