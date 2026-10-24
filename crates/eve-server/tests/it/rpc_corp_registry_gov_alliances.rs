//! Authentic state-backed integration tests for `corpRegistry` welcome mail, structure transfer settings, alliances, wars, and aggression settings:
//! - Welcome mail & structures: `SetCorpWelcomeMail`, `GetCorpWelcomeMail`, `RegisterNewCorpMailRestrictionSettings`, `DoesCorpRestrictCorpMails`, `RegisterNewAcceptStructureSettings`, `DoesMyCorpAcceptStructures`, `SetStructureReinforceDefault`, `GetStructureReinforceDefault`
//! - Alliances, wars & abilities: `CharGetAllyBaseCost`, `UpdateCorporationAbilities`, `GetAggressionSettings`, `RegisterNewAggressionSettings`, `CreateAlliance`, `ApplyToJoinAlliance`, `GetAllianceApplications`, `DeleteAllianceApplication`, `DeclareWarAgainst`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{IskCents, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

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
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
}

fn set_corp_balance(ctx: &ServerContext, corp_id: i64, isk: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_f64_isk(isk);
            corp.master_wallet = IskCents::from_f64_isk(isk);
        }
    });
}

fn create_test_structure(id: i64, corp_id: i64, name: &str) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id: 35832, // Astrahus
        solar_system_id: 30_000_142,
        owner_corp_id: corp_id,
        name: name.into(),
        position: [200_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        hull_hp: 5_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
    }
}

#[tokio::test]
async fn test_corp_welcome_mail_and_structures() {
    // parity: rpc:corpRegistry.SetCorpWelcomeMail
    // parity: rpc:corpRegistry.GetCorpWelcomeMail
    // parity: rpc:corpRegistry.RegisterNewCorpMailRestrictionSettings
    // parity: rpc:corpRegistry.DoesCorpRestrictCorpMails
    // parity: rpc:corpRegistry.RegisterNewAcceptStructureSettings
    // parity: rpc:corpRegistry.DoesMyCorpAcceptStructures
    // parity: rpc:corpRegistry.SetStructureReinforceDefault
    // parity: rpc:corpRegistry.GetStructureReinforceDefault

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 61_004, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 61_007, "other", "other-secret", PILOT_2).await;

    let created1 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Sovereign Communications".into()),
            PyRep::String("SOVCOM".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 1");
    let corp1_id = field(&created1, "corporationID")
        .as_i64()
        .expect("corporationID 1");

    let created2 = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Structure Recipient Corp".into()),
            PyRep::String("SRCP".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 2");
    let corp2_id = field(&created2, "corporationID")
        .as_i64()
        .expect("corporationID 2");

    // 1. SetCorpWelcomeMail & GetCorpWelcomeMail
    let mail_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[
            PyRep::String("Welcome to SovCom".into()),
            PyRep::String("Please configure Discord comms immediately".into()),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("SetCorpWelcomeMail");
    assert_eq!(field(&mail_res, "enabled"), PyRep::Bool(true));

    let welcome = rpc(&ctx, &mut sess1, "corpRegistry", "GetCorpWelcomeMail", &[])
        .await
        .expect("GetCorpWelcomeMail");
    assert_eq!(field(&welcome, "enabled"), PyRep::Bool(true));
    assert_eq!(
        field(&welcome, "subject"),
        PyRep::String("Welcome to SovCom".into())
    );

    // 2. RegisterNewCorpMailRestrictionSettings & DoesCorpRestrictCorpMails
    let restrict = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewCorpMailRestrictionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewCorpMailRestrictionSettings");
    assert_eq!(restrict, PyRep::Bool(true));

    let does_restrict = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "DoesCorpRestrictCorpMails",
        &[],
    )
    .await
    .expect("DoesCorpRestrictCorpMails");
    assert_eq!(does_restrict, PyRep::Bool(true));

    // 3. DoesMyCorpAcceptStructures default true -> RegisterNewAcceptStructureSettings(false) -> blocks structure transfer
    let default_accept = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "DoesMyCorpAcceptStructures",
        &[],
    )
    .await
    .expect("DoesMyCorpAcceptStructures default");
    assert_eq!(default_accept, PyRep::Bool(true));

    let accept_struct = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings false");
    assert_eq!(accept_struct, PyRep::Bool(false));

    let does_accept = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "DoesMyCorpAcceptStructures",
        &[],
    )
    .await
    .expect("DoesMyCorpAcceptStructures after false");
    assert_eq!(does_accept, PyRep::Bool(false));

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            995_000_201,
            create_test_structure(995_000_201, corp1_id, "Transfer Test Astrahus"),
        );
    });
    assert!(
        rpc(
            &ctx,
            &mut sess1,
            "beyonce",
            "CmdTransferCorpAssetOwnership",
            &[PyRep::Integer(995_000_201), PyRep::Integer(corp2_id)]
        )
        .await
        .is_err(),
        "structure transfer to a corp with accept_structures=false must be rejected"
    );

    let reenable_accept = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings true");
    assert_eq!(reenable_accept, PyRep::Bool(true));
    let does_accept_after_true = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "DoesMyCorpAcceptStructures",
        &[],
    )
    .await
    .expect("DoesMyCorpAcceptStructures after true");
    assert_eq!(does_accept_after_true, PyRep::Bool(true));

    // 4. SetStructureReinforceDefault & GetStructureReinforceDefault
    let reinforce_hour = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(20)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(reinforce_hour, PyRep::Integer(20));

    let get_reinforce = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetStructureReinforceDefault",
        &[],
    )
    .await
    .expect("GetStructureReinforceDefault");
    assert_eq!(get_reinforce, PyRep::Integer(20));
}

#[tokio::test]
async fn test_corp_alliances_wars_and_aggression() {
    // parity: rpc:corpRegistry.CharGetAllyBaseCost
    // parity: rpc:corpRegistry.UpdateCorporationAbilities
    // parity: rpc:corpRegistry.GetAggressionSettings
    // parity: rpc:corpRegistry.RegisterNewAggressionSettings
    // parity: rpc:corpRegistry.CreateAlliance
    // parity: rpc:corpRegistry.ApplyToJoinAlliance
    // parity: rpc:corpRegistry.GetAllianceApplications
    // parity: rpc:corpRegistry.DeleteAllianceApplication
    // parity: rpc:corpRegistry.DeclareWarAgainst

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 61_005, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 61_006, "other", "other-secret", PILOT_2).await;

    let c1 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Aggressor Cartel".into()),
            PyRep::String("AGGC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 1");
    let corp1_id = field(&c1, "corporationID").as_i64().expect("corp1_id");

    let c2 = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Defender Syndicate".into()),
            PyRep::String("DEFS".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 2");
    let corp2_id = field(&c2, "corporationID").as_i64().expect("corp2_id");

    // 1. CharGetAllyBaseCost & UpdateCorporationAbilities
    let ally_cost = rpc(&ctx, &mut sess1, "corpRegistry", "CharGetAllyBaseCost", &[])
        .await
        .expect("CharGetAllyBaseCost");
    assert_eq!(ally_cost, PyRep::Real(10_000_000.0));

    let abil = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateCorporationAbilities",
        &[],
    )
    .await
    .expect("UpdateCorporationAbilities");
    assert_eq!(field(&abil, "corporationID"), PyRep::Integer(corp1_id));

    // 2. GetAggressionSettings & RegisterNewAggressionSettings (verify V24 _enableAfter/_disableAfter)
    let agg = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp1_id)],
    )
    .await
    .expect("GetAggressionSettings");
    assert_eq!(field(&agg, "corporationID"), PyRep::Integer(corp1_id));
    assert_eq!(field(&agg, "_enableAfter"), PyRep::None);
    assert_eq!(field(&agg, "_disableAfter"), PyRep::Integer(0));

    let reg_agg = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAggressionSettings");
    assert_eq!(field(&reg_agg, "isFriendlyFireLegal"), PyRep::Bool(true));
    assert!(field(&reg_agg, "_enableAfter").as_i64().unwrap_or(0) > 0);

    let agg_before_24h = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp1_id)],
    )
    .await
    .expect("GetAggressionSettings before 24h");
    assert_eq!(
        field(&agg_before_24h, "isFriendlyFireLegal"),
        PyRep::Bool(false),
        "friendly fire must remain false before 24h delay expires"
    );
    assert_eq!(
        field(&agg_before_24h, "pendingFriendlyFireLegal"),
        PyRep::Bool(true)
    );
    assert!(field(&agg_before_24h, "_enableAfter").as_i64().unwrap_or(0) > 0);

    ctx.advance_secs(86_401);
    let agg_after_24h = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp1_id)],
    )
    .await
    .expect("GetAggressionSettings after 24h");
    assert_eq!(
        field(&agg_after_24h, "isFriendlyFireLegal"),
        PyRep::Bool(true),
        "friendly fire must promote to true after 24h delay expires"
    );
    assert_eq!(field(&agg_after_24h, "pendingFriendlyFireLegal"), PyRep::None);
    assert_eq!(field(&agg_after_24h, "_enableAfter"), PyRep::Integer(0));
    let (ff_db, pending_ff_db) = ctx.db.with_snapshot(|snap| {
        snap.corp_governance
            .get(&corp1_id)
            .map(|g| (g.friendly_fire_legal, g.pending_friendly_fire))
            .unwrap_or((false, None))
    });
    assert!(ff_db && pending_ff_db.is_none());

    // 3. CreateAlliance (requires 1B ISK in corp wallet div 1, records journal entry, rejects AlreadyInAlliance)
    assert!(
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "CreateAlliance",
            &[
                PyRep::String("Unfunded Alliance".into()),
                PyRep::String("UNFND".into()),
            ],
        )
        .await
        .is_err(),
        "CreateAlliance without 1B ISK in corp master wallet must fail"
    );

    set_corp_balance(&ctx, corp1_id, 1_500_000_000.0);
    let ally = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Northern Coalition Force".into()),
            PyRep::String("NCF".into()),
            PyRep::String("Sovereign nullsec coalition".into()),
            PyRep::String("https://ncf.example".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let alliance_id = field(&ally, "allianceID").as_i64().expect("allianceID");
    assert!(alliance_id >= 99_000_000);
    assert_eq!(sess1.alliance_id, Some(alliance_id));

    let (rem_bal, has_ally_journal) = ctx.db.with_snapshot(|snap| {
        let bal = snap
            .corporations
            .get(&corp1_id)
            .map(|c| c.wallet_divisions[0].balance.to_f64_isk())
            .unwrap_or(0.0);
        let j = snap
            .corp_wallet_journals
            .get(&(corp1_id, 1))
            .map(|entries| {
                entries
                    .iter()
                    .any(|e| e.ref_type == "alliance_creation_fee" && (e.amount - (-1_000_000_000.0)).abs() < 1.0)
            })
            .unwrap_or(false);
        (bal, j)
    });
    assert!((rem_bal - 500_000_000.0).abs() < 1.0);
    assert!(has_ally_journal, "expected alliance_creation_fee journal entry");

    assert!(
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "CreateAlliance",
            &[
                PyRep::String("Duplicate Alliance".into()),
                PyRep::String("DUPA".into()),
            ],
        )
        .await
        .is_err(),
        "corp already in an alliance must be rejected with AlreadyInAlliance"
    );

    // 4. ApplyToJoinAlliance, GetAllianceApplications, DeleteAllianceApplication
    let join_app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "ApplyToJoinAlliance",
        &[
            PyRep::Integer(alliance_id),
            PyRep::String("Defender Syndicate application".into()),
        ],
    )
    .await
    .expect("ApplyToJoinAlliance");
    assert_eq!(
        field(&join_app, "corporationID"),
        PyRep::Integer(corp2_id)
    );

    let ally_apps = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetAllianceApplications",
        &[PyRep::Integer(alliance_id)],
    )
    .await
    .expect("GetAllianceApplications");
    let (_, aa_rows) = rowset_data(&ally_apps);
    assert!(aa_rows.iter().any(|r| r[1] == PyRep::Integer(corp2_id)));

    let del_app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "DeleteAllianceApplication",
        &[PyRep::Integer(alliance_id)],
    )
    .await
    .expect("DeleteAllianceApplication");
    assert_eq!(field(&del_app, "deleted"), PyRep::Bool(true));

    // 5. DeclareWarAgainst
    set_corp_balance(&ctx, corp1_id, 1_000_000_000.0);
    set_corp_balance(&ctx, corp2_id, 1_000_000_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            995_000_101,
            create_test_structure(995_000_101, corp1_id, "Aggressor War HQ"),
        );
        snap.structures.insert(
            995_000_102,
            create_test_structure(995_000_102, corp2_id, "Defender Citadel"),
        );
    });

    let war_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "DeclareWarAgainst",
        &[
            PyRep::Integer(corp2_id),
            PyRep::Integer(995_000_101),
        ],
    )
    .await
    .expect("DeclareWarAgainst");
    let war_id = field(&war_res, "warID").as_i64().expect("warID");
    assert!(war_id > 0);
}
