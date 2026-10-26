//! Authentic state-backed integration tests for `corpRegistry` shares, dividends, titles, bulletins, contacts, and labels:
//! - Shares & dividends: `MoveCompanyShares`, `MovePrivateShares`, `GetShareholders`, `GetSharesByShareholder`, `GetMemberIDsWithMoreThanAvgShares`, `PayoutDividend`, `ResignFromCEO`, `GetNumberOfPotentialCEOs`
//! - Titles & bulletins: `UpdateTitle`, `UpdateTitles`, `GetTitles`, `DeleteTitle`, `AddBulletin`, `UpdateBulletinOrder`, `GetBulletins`, `DeleteBulletin`
//! - Contacts & labels: `AddCorporateContact`, `EditCorporateContact`, `EditContactsRelationshipID`, `RemoveCorporateContacts`, `GetCorporateContacts`, `CreateLabel`, `EditLabel`, `GetLabels`, `DeleteLabel`, `AssignLabels`, `RemoveLabels`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::IskCents;
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

    // 1. MoveCompanyShares
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
    assert_eq!(field(&move_corp, "sharesTransferred"), PyRep::Integer(200));

    // 2. MovePrivateShares
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
    assert_eq!(bsh_rows.len(), 1);
    assert_eq!(bsh_rows[0][2], PyRep::Integer(200));

    let by_sh_p3 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3)],
    )
    .await
    .expect("GetSharesByShareholder PILOT_3");
    let (_, bsh_p3_rows) = rowset_data(&by_sh_p3);
    assert_eq!(bsh_p3_rows.len(), 1);
    assert_eq!(bsh_p3_rows[0][0], PyRep::Integer(PILOT_3));
    assert_eq!(bsh_p3_rows[0][2], PyRep::Integer(50));

    let by_sh_none = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::Integer(corp_id), PyRep::Integer(999_999)],
    )
    .await
    .expect("GetSharesByShareholder non-holder");
    let (_, bsh_none_rows) = rowset_data(&by_sh_none);
    assert!(bsh_none_rows.is_empty());

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
            PyRep::Bool(true),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("PayoutDividend");
    assert_eq!(field(&div_res, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&div_res, "totalPayoutISK"), PyRep::Real(1_000_000.0));

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

    // 8. ResignFromCEO
    let resign = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "ResignFromCEO",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("ResignFromCEO");
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(PILOT_2));

    // 9. Issue #43 security checks: non-director cannot MoveCompanyShares; cannot exceed treasury balance
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_1, 0);
        }
    });
    let err_non_dir = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "MoveCompanyShares",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(PILOT_3),
            PyRep::Integer(10),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_dir.code, "CorpRoleDenied");

    let mut sess2 = SessionState::new(61_099);
    sess2.user_id = Some(2);
    sess2.character_id = Some(PILOT_2);
    sess2.corporation_id = Some(corp_id);
    let err_insuff_corp = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "MoveCompanyShares",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(PILOT_3),
            PyRep::Integer(10_000),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_insuff_corp.code, "InsufficientShares");

    let err_insuff_priv = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "MovePrivateShares",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(PILOT_3),
            PyRep::Integer(10_000),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_insuff_priv.code, "InsufficientShares");
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

    // 1. UpdateTitle & UpdateTitles & GetTitles & DeleteTitle
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

    let titles = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetTitles",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetTitles");
    let (_, t_rows) = rowset_data(&titles);
    assert_eq!(t_rows.len(), 2);

    let del_t = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteTitle",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("DeleteTitle");
    assert_eq!(field(&del_t, "deleted"), PyRep::Bool(true));

    // 2. AddBulletin & DeleteBulletin (monotonic ID allocation + BulletinNotFound error)
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
    .expect("AddBulletin 1");
    assert_eq!(field(&b1, "bulletinID"), PyRep::Integer(1));

    let b2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Mining Fleet Schedule".into()),
            PyRep::String("Moon mining belt 2 tomorrow morning".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin 2");
    assert_eq!(field(&b2, "bulletinID"), PyRep::Integer(2));

    // Delete bulletin 2 and add a third bulletin: ID must be monotonic (3, not 2)
    let del_b2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteBulletin",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("DeleteBulletin 2");
    assert_eq!(field(&del_b2, "bulletinID"), PyRep::Integer(2));
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "DeleteBulletin",
            &[PyRep::Integer(999_999)]
        )
        .await
        .is_err(),
        "deleting unknown bulletin must return BulletinNotFound"
    );

    let b3 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Logistics Convoy".into()),
            PyRep::String("Jump freighters depart at 22:00 UTC".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin 3");
    assert_eq!(
        field(&b3, "bulletinID"),
        PyRep::Integer(3),
        "bulletin IDs must be monotonic after deletion"
    );

    // 3. UpdateBulletinOrder (V24 list of (bulletinID, sortOrder) pairs)
    let reorder_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateBulletinOrder",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(10)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(20)]),
        ])],
    )
    .await
    .expect("UpdateBulletinOrder");
    assert_eq!(reorder_res, PyRep::Bool(true));

    // 4. GetBulletins (verify sortOrder column and reordered rows [3, 1])
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
    assert!(b_cols.contains(&"sortOrder".to_string()));
    assert_eq!(b_rows.len(), 2);
    assert_eq!(b_rows[0][0], PyRep::Integer(3));
    assert_eq!(b_rows[1][0], PyRep::Integer(1));

    // 5. RegisterNewAggressionSettings + GetAggressionSettings 24h delay & RegisterNewAcceptStructureSettings + DoesMyCorpAcceptStructures
    let reg_agg = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAggressionSettings");
    assert!(field(&reg_agg, "_enableAfter").as_i64().unwrap_or(0) > 0);
    let agg_before = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettings before 24h");
    assert_eq!(field(&agg_before, "isFriendlyFireLegal"), PyRep::Bool(false));
    assert_eq!(field(&agg_before, "pendingFriendlyFireLegal"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    let agg_after = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettings after 24h");
    assert_eq!(field(&agg_after, "isFriendlyFireLegal"), PyRep::Bool(true));
    assert_eq!(field(&agg_after, "pendingFriendlyFireLegal"), PyRep::None);
    let (ff_persisted, ff_pending) = ctx.db.with_snapshot(|snap| {
        snap.corp_governance
            .get(&corp_id)
            .map(|g| (g.friendly_fire_legal, g.pending_friendly_fire))
            .unwrap_or((false, None))
    });
    assert!(ff_persisted && ff_pending.is_none());

    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAcceptStructureSettings", &[PyRep::Bool(false)]).await.unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAcceptStructureSettings", &[PyRep::Bool(true)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(true)
    );
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

    let created = rpc(&ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Diplomatic League".into()), PyRep::String("DLEAG".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let _corp_id = field(&created, "corporationID").as_i64().expect("corporationID");

    // Seed PILOT_3 so PILOT_3 is a valid owner ID
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch3) = snap.characters.get(&PILOT_2).cloned() {
            ch3.character_id = PILOT_3;
            ch3.name = "Diplomat Three".into();
            snap.characters.insert(PILOT_3, ch3);
        }
    });

    // 1. AddCorporateContact: duplicate and invalid owner checks
    let c1 = rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(5.0), PyRep::Integer(1)]).await.expect("AddCorporateContact PILOT_2");
    assert_eq!(field(&c1, "contactID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&c1, "relationshipID"), PyRep::Real(5.0));

    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(5.0)]).await.is_err(), "duplicate AddCorporateContact must fail");
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(999_888_777), PyRep::Real(5.0)]).await.is_err(), "unknown owner ID must fail");

    // 2. EditCorporateContact: missing contact check + update
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(PILOT_3), PyRep::Real(10.0)]).await.is_err(), "editing non-existent contact must fail");
    let c2 = rpc(&ctx, &mut sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(10.0), PyRep::Integer(1)]).await.expect("EditCorporateContact");
    assert_eq!(field(&c2, "relationshipID"), PyRep::Real(10.0));

    // Add PILOT_3 as second contact for batch operations
    rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_3), PyRep::Real(0.0)]).await.expect("AddCorporateContact PILOT_3");

    // 3. EditContactsRelationshipID (V24 batch list [PILOT_2, PILOT_3] + missing contact check)
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "EditContactsRelationshipID", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Real(-5.0)]).await.is_err());
    let c3 = rpc(&ctx, &mut sess, "corpRegistry", "EditContactsRelationshipID", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Real(-5.0)]).await.expect("EditContactsRelationshipID batch");
    assert_eq!(field(&c3, "relationshipID"), PyRep::Real(-5.0));

    // 4. CreateLabel, EditLabel, GetLabels
    let l1 = rpc(&ctx, &mut sess, "corpRegistry", "CreateLabel", &[PyRep::String("Strategic Allies".into()), PyRep::Integer(0x00FF00)]).await.expect("CreateLabel");
    let label_id = field(&l1, "labelID").as_i64().expect("labelID");
    assert!(label_id > 0);

    let l2 = rpc(&ctx, &mut sess, "corpRegistry", "EditLabel", &[PyRep::Integer(label_id), PyRep::String("Coalition Partners".into()), PyRep::Integer(0x0000FF)]).await.expect("EditLabel");
    assert_eq!(field(&l2, "name"), PyRep::String("Coalition Partners".into()));

    let labels = rpc(&ctx, &mut sess, "corpRegistry", "GetLabels", &[]).await.expect("GetLabels");
    let (_, l_rows) = rowset_data(&labels);
    assert_eq!(l_rows.len(), 1);

    // 5. AssignLabels & RemoveLabels across multiple contacts + ContactNotFound checks
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Integer(label_id)]).await.is_err());
    rpc(&ctx, &mut sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Integer(label_id)]).await.expect("AssignLabels batch");

    let contacts = rpc(&ctx, &mut sess, "corpRegistry", "GetCorporateContacts", &[]).await.expect("GetCorporateContacts after AssignLabels");
    let (_, c_rows) = rowset_data(&contacts);
    assert_eq!(c_rows.len(), 2);
    assert!(c_rows.iter().all(|r| r[1] == PyRep::Real(-5.0)));
    assert!(c_rows.iter().all(|r| r[2] == PyRep::Integer(label_id)));

    assert!(rpc(&ctx, &mut sess, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Integer(label_id)]).await.is_err());
    rpc(&ctx, &mut sess, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Integer(label_id)]).await.expect("RemoveLabels batch");

    // 6. DeleteLabel + LabelNotFound check + RemoveCorporateContacts
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(999_999)]).await.is_err());
    let del_l = rpc(&ctx, &mut sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(label_id)]).await.expect("DeleteLabel");
    assert_eq!(field(&del_l, "labelID"), PyRep::Integer(label_id));

    let rem_c = rpc(&ctx, &mut sess, "corpRegistry", "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)])]).await.expect("RemoveCorporateContacts");
    assert_eq!(field(&rem_c, "removed"), PyRep::Bool(true));
}
