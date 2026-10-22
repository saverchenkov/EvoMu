//! Authentic state-backed integration tests for `corpRegistry` governance, social, and alliance RPC methods:
//! - Shares & dividends: `MoveCompanyShares`, `MovePrivateShares`, `GetShareholders`, `GetSharesByShareholder`, `GetMemberIDsWithMoreThanAvgShares`, `PayoutDividend`, `ResignFromCEO`, `GetNumberOfPotentialCEOs`
//! - Titles & bulletins: `UpdateTitle`, `UpdateTitles`, `GetTitles`, `DeleteTitle`, `AddBulletin`, `UpdateBulletinOrder`, `GetBulletins`, `DeleteBulletin`
//! - Contacts & labels: `AddCorporateContact`, `EditCorporateContact`, `EditContactsRelationshipID`, `RemoveCorporateContacts`, `GetCorporateContacts`, `CreateLabel`, `EditLabel`, `GetLabels`, `DeleteLabel`, `AssignLabels`, `RemoveLabels`
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
const PILOT_3: i64 = 90_000_003;

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
async fn test_corp_shares_dividends_and_ceo_transition() {
    // parity: rpc:corpRegistry.MoveCompanyShares
    // parity: rpc:corpRegistry.MovePrivateShares
    // parity: rpc:corpRegistry.GetShareholders
    // parity: rpc:corpRegistry.GetSharesByShareholder
    // parity: rpc:corpRegistry.GetMemberIDsWithMoreThanAvgShares
    // parity: rpc:corpRegistry.PayoutDividend
    // parity: rpc:corpRegistry.ResignFromCEO
    // parity: rpc:corpRegistry.GetNumberOfPotentialCEOs

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 61_001, "demo", "demo-secret", PILOT_1).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Titan Holdings".into()),
            PyRep::String("TITAN".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. MoveCompanyShares (transfer 200 shares to pilot 2)
    let move_corp = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "MoveCompanyShares",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(200),
        ],
    )
    .await
    .expect("MoveCompanyShares");
    assert_eq!(
        field(&move_corp, "sharesTransferred"),
        PyRep::Integer(200)
    );
    assert_eq!(
        field(&move_corp, "toShareholderID"),
        PyRep::Integer(PILOT_2)
    );

    // 2. MovePrivateShares (transfer 50 shares to pilot 3)
    let move_priv = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "MovePrivateShares",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(PILOT_3),
            PyRep::Integer(50),
        ],
    )
    .await
    .expect("MovePrivateShares");
    assert_eq!(field(&move_priv, "sharesTransferred"), PyRep::Integer(50));

    // 3. GetShareholders
    let sh_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetShareholders",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetShareholders");
    let (s_cols, s_rows) = rowset_data(&sh_res);
    assert!(s_cols.contains(&"shareholderID".to_string()));
    assert!(s_rows.len() >= 2);

    // 4. GetSharesByShareholder
    let by_sh = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("GetSharesByShareholder");
    let (_, bsh_rows) = rowset_data(&by_sh);
    assert!(bsh_rows.len() >= 2);

    // 5. GetMemberIDsWithMoreThanAvgShares
    let avg_sh = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMemberIDsWithMoreThanAvgShares",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetMemberIDsWithMoreThanAvgShares");
    let PyRep::List(sh_list) = avg_sh else { panic!("expected list") };
    assert!(sh_list.contains(&PyRep::Integer(PILOT_1)));

    // 6. PayoutDividend
    set_corp_balance(&ctx, corp_id, 10_000_000.0);
    let div_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "PayoutDividend",
        &[
            PyRep::Real(1_000_000.0),
            PyRep::Bool(true), // to shareholders
            PyRep::Integer(1), // wallet division 1
        ],
    )
    .await
    .expect("PayoutDividend");
    assert_eq!(field(&div_res, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&div_res, "totalPayoutISK"), PyRep::Real(1_000_000.0));
    assert!(field(&div_res, "recipientCount").as_i64().unwrap() >= 2);

    // Add pilot 2 to corporation members for potential CEO tests
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(PILOT_2);
            corp.member_roles.insert(PILOT_2, 0);
        }
    });

    // 7. GetNumberOfPotentialCEOs
    let num_ceos = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetNumberOfPotentialCEOs",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetNumberOfPotentialCEOs");
    assert_eq!(num_ceos, PyRep::Integer(1));

    // 8. ResignFromCEO (hand over leadership to pilot 2)
    let resign = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "ResignFromCEO",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("ResignFromCEO");
    assert_eq!(field(&resign, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(PILOT_2));
}

#[tokio::test]
async fn test_corp_titles_and_bulletins() {
    // parity: rpc:corpRegistry.UpdateTitle
    // parity: rpc:corpRegistry.UpdateTitles
    // parity: rpc:corpRegistry.GetTitles
    // parity: rpc:corpRegistry.DeleteTitle
    // parity: rpc:corpRegistry.AddBulletin
    // parity: rpc:corpRegistry.UpdateBulletinOrder
    // parity: rpc:corpRegistry.GetBulletins
    // parity: rpc:corpRegistry.DeleteBulletin

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 61_002, "demo", "demo-secret", PILOT_1).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Vanguard Command".into()),
            PyRep::String("VGCMD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. UpdateTitle (Create Title 1)
    let t1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(1),
            PyRep::String("Fleet Commander".into()),
            PyRep::Integer(1i64 << 15),
        ],
    )
    .await
    .expect("UpdateTitle");
    assert_eq!(field(&t1, "titleID"), PyRep::Integer(1));
    assert_eq!(
        field(&t1, "titleName"),
        PyRep::String("Fleet Commander".into())
    );

    // 2. UpdateTitles (Create Title 2)
    let t2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitles",
        &[
            PyRep::Integer(2),
            PyRep::String("Chief Engineer".into()),
            PyRep::Integer(1i64 << 22),
        ],
    )
    .await
    .expect("UpdateTitles");
    assert_eq!(field(&t2, "titleID"), PyRep::Integer(2));
    assert_eq!(
        field(&t2, "titleName"),
        PyRep::String("Chief Engineer".into())
    );

    // 3. GetTitles
    let titles = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetTitles",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetTitles");
    let (t_cols, t_rows) = rowset_data(&titles);
    assert_eq!(t_cols, vec!["titleID", "titleName", "roles"]);
    assert_eq!(t_rows.len(), 2);

    // 4. DeleteTitle (Delete Title 1)
    let del_t = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteTitle",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("DeleteTitle");
    assert_eq!(field(&del_t, "titleID"), PyRep::Integer(1));
    assert_eq!(field(&del_t, "deleted"), PyRep::Bool(true));

    // 5. AddBulletin
    let b1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Strategic Deployment".into()),
            PyRep::String("All pilots form up at 20:00 UTC".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin");
    assert_eq!(field(&b1, "bulletinID"), PyRep::Integer(1));
    assert_eq!(
        field(&b1, "title"),
        PyRep::String("Strategic Deployment".into())
    );

    // 6. UpdateBulletinOrder (Add a second bulletin)
    let b2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateBulletinOrder",
        &[
            PyRep::String("Mining Fleet Schedule".into()),
            PyRep::String("Moon mining belt 2 tomorrow morning".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("UpdateBulletinOrder");
    assert_eq!(field(&b2, "bulletinID"), PyRep::Integer(2));

    // 7. GetBulletins
    let bulletins = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetBulletins",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetBulletins");
    let (b_cols, b_rows) = rowset_data(&bulletins);
    assert!(b_cols.contains(&"bulletinID".to_string()));
    assert_eq!(b_rows.len(), 2);

    // 8. DeleteBulletin (Delete Bulletin 1)
    let del_b = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteBulletin",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("DeleteBulletin");
    assert_eq!(field(&del_b, "bulletinID"), PyRep::Integer(1));
    assert_eq!(field(&del_b, "deleted"), PyRep::Bool(true));
}

#[tokio::test]
async fn test_corp_contacts_and_labels() {
    // parity: rpc:corpRegistry.AddCorporateContact
    // parity: rpc:corpRegistry.EditCorporateContact
    // parity: rpc:corpRegistry.EditContactsRelationshipID
    // parity: rpc:corpRegistry.RemoveCorporateContacts
    // parity: rpc:corpRegistry.GetCorporateContacts
    // parity: rpc:corpRegistry.CreateLabel
    // parity: rpc:corpRegistry.EditLabel
    // parity: rpc:corpRegistry.GetLabels
    // parity: rpc:corpRegistry.DeleteLabel
    // parity: rpc:corpRegistry.AssignLabels
    // parity: rpc:corpRegistry.RemoveLabels

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 61_003, "demo", "demo-secret", PILOT_1).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Diplomatic League".into()),
            PyRep::String("DLEAG".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let _corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. AddCorporateContact
    let c1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporateContact",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Real(5.0),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("AddCorporateContact");
    assert_eq!(field(&c1, "contactID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&c1, "relationshipID"), PyRep::Real(5.0));

    // 2. EditCorporateContact
    let c2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "EditCorporateContact",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Real(10.0),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("EditCorporateContact");
    assert_eq!(field(&c2, "relationshipID"), PyRep::Real(10.0));

    // 3. EditContactsRelationshipID
    let c3 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "EditContactsRelationshipID",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Real(-5.0),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("EditContactsRelationshipID");
    assert_eq!(field(&c3, "relationshipID"), PyRep::Real(-5.0));

    // 4. GetCorporateContacts
    let contacts = rpc(&ctx, &mut sess, "corpRegistry", "GetCorporateContacts", &[])
        .await
        .expect("GetCorporateContacts");
    let (c_cols, c_rows) = rowset_data(&contacts);
    assert_eq!(c_cols, vec!["contactID", "relationshipID", "labelMask"]);
    assert_eq!(c_rows.len(), 1);

    // 5. RemoveCorporateContacts
    let rem_c = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "RemoveCorporateContacts",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("RemoveCorporateContacts");
    assert_eq!(field(&rem_c, "contactID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&rem_c, "removed"), PyRep::Bool(true));

    // 6. CreateLabel
    let l1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "CreateLabel",
        &[
            PyRep::String("Strategic Allies".into()),
            PyRep::Integer(0x00FF00),
        ],
    )
    .await
    .expect("CreateLabel");
    let label_id = field(&l1, "labelID").as_i64().expect("labelID");
    assert!(label_id > 0);
    assert_eq!(
        field(&l1, "name"),
        PyRep::String("Strategic Allies".into())
    );

    // 7. EditLabel
    let l2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "EditLabel",
        &[
            PyRep::Integer(label_id),
            PyRep::String("Coalition Partners".into()),
            PyRep::Integer(0x0000FF),
        ],
    )
    .await
    .expect("EditLabel");
    assert_eq!(
        field(&l2, "name"),
        PyRep::String("Coalition Partners".into())
    );

    // 8. GetLabels
    let labels = rpc(&ctx, &mut sess, "corpRegistry", "GetLabels", &[])
        .await
        .expect("GetLabels");
    let (l_cols, l_rows) = rowset_data(&labels);
    assert_eq!(l_cols, vec!["labelID", "name", "color"]);
    assert_eq!(l_rows.len(), 1);

    // 9. AssignLabels
    let assign = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            PyRep::Integer(label_id),
        ],
    )
    .await
    .expect("AssignLabels");
    assert_eq!(field(&assign, "labelID"), PyRep::Integer(label_id));

    // 10. RemoveLabels
    let rem_l = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "RemoveLabels",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            PyRep::Integer(label_id),
        ],
    )
    .await
    .expect("RemoveLabels");
    assert_eq!(field(&rem_l, "labelID"), PyRep::Integer(label_id));

    // 11. DeleteLabel
    let del_l = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteLabel",
        &[PyRep::Integer(label_id)],
    )
    .await
    .expect("DeleteLabel");
    assert_eq!(field(&del_l, "labelID"), PyRep::Integer(label_id));
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
    let mut sess = select_pilot(&ctx, 61_004, "demo", "demo-secret", PILOT_1).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Sovereign Communications".into()),
            PyRep::String("SOVCOM".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let _corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. SetCorpWelcomeMail
    let mail_res = rpc(
        &ctx,
        &mut sess,
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
    assert_eq!(
        field(&mail_res, "subject"),
        PyRep::String("Welcome to SovCom".into())
    );

    // 2. GetCorpWelcomeMail
    let welcome = rpc(&ctx, &mut sess, "corpRegistry", "GetCorpWelcomeMail", &[])
        .await
        .expect("GetCorpWelcomeMail");
    assert_eq!(field(&welcome, "enabled"), PyRep::Bool(true));
    assert_eq!(
        field(&welcome, "subject"),
        PyRep::String("Welcome to SovCom".into())
    );

    // 3. RegisterNewCorpMailRestrictionSettings
    let restrict = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "RegisterNewCorpMailRestrictionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewCorpMailRestrictionSettings");
    assert_eq!(restrict, PyRep::Bool(true));

    // 4. DoesCorpRestrictCorpMails
    let does_restrict = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DoesCorpRestrictCorpMails",
        &[],
    )
    .await
    .expect("DoesCorpRestrictCorpMails");
    assert_eq!(does_restrict, PyRep::Bool(true));

    // 5. RegisterNewAcceptStructureSettings
    let accept_struct = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings");
    assert_eq!(accept_struct, PyRep::Bool(false));

    // 6. DoesMyCorpAcceptStructures
    let does_accept = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DoesMyCorpAcceptStructures",
        &[],
    )
    .await
    .expect("DoesMyCorpAcceptStructures");
    assert_eq!(does_accept, PyRep::Bool(false));

    // 7. SetStructureReinforceDefault
    let reinforce_hour = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(20)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(reinforce_hour, PyRep::Integer(20));

    // 8. GetStructureReinforceDefault
    let get_reinforce = rpc(
        &ctx,
        &mut sess,
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

    // Create corporation for pilot 1
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

    // Create corporation for pilot 2
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

    // 1. CharGetAllyBaseCost
    let ally_cost = rpc(&ctx, &mut sess1, "corpRegistry", "CharGetAllyBaseCost", &[])
        .await
        .expect("CharGetAllyBaseCost");
    assert_eq!(ally_cost, PyRep::Real(10_000_000.0));

    // 2. UpdateCorporationAbilities
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
    assert!(field(&abil, "memberLimit").as_i64().unwrap() >= 1);

    // 3. GetAggressionSettings
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

    // 4. RegisterNewAggressionSettings
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
    assert_eq!(field(&reg_agg, "corporationID"), PyRep::Integer(corp1_id));

    // 5. CreateAlliance
    let ally = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Northern Coalition Force".into()),
            PyRep::String("NCF".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let alliance_id = field(&ally, "allianceID").as_i64().expect("allianceID");
    assert!(alliance_id >= 99_000_000);

    // 6. ApplyToJoinAlliance (Corp 2 applies to join alliance)
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
    assert_eq!(
        field(&join_app, "allianceID"),
        PyRep::Integer(alliance_id)
    );

    // 7. GetAllianceApplications (Alliance executor reads apps)
    let ally_apps = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetAllianceApplications",
        &[PyRep::Integer(alliance_id)],
    )
    .await
    .expect("GetAllianceApplications");
    let (aa_cols, aa_rows) = rowset_data(&ally_apps);
    assert!(aa_cols.contains(&"allianceID".to_string()));
    assert!(aa_rows.iter().any(|r| r[1] == PyRep::Integer(corp2_id)));

    // 8. DeleteAllianceApplication
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

    // 9. DeclareWarAgainst (Setup War HQ and eligible structures)
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
    assert_eq!(field(&war_res, "warHQ"), PyRep::Integer(995_000_101));
}
