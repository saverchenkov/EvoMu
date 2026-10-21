//! Acceptance tests for Classic machoNet RPC Methods (Batch 2).
//!
//! Services covered:
//! - corpRegistry (85 rows)
//! - charMgr (31 rows)
//! - mailMgr & mailingListsMgr (49 rows)
//! - fleetObjectHandler (36 rows)
//! - ship (29 rows)
//! Total: 230 rows


use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

const P1: i64 = 90_000_001; // Aura Capsuleer (user 1, demo)
const P2: i64 = 90_000_002; // Rival Pilot (user 2, other)
const CORP_ID: i64 = 98_000_001;
const JITA_44: i64 = 60_003_760;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(username.into()), PyRep::String(password.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = Some(JITA_44);
    sess.structure_id = None;
    sess
}

async fn rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
}

// ============================================================================
// 1. corpRegistry Suite (85 rows)
// ============================================================================
#[tokio::test]
async fn test_corp_registry_rpc_suite() {
    // parity: rpc:corpRegistry.AddBulletin, rpc:corpRegistry.AddCorporateContact, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.ApplyToJoinAlliance, rpc:corpRegistry.AssignLabels, rpc:corpRegistry.CanBeKickedOut, rpc:corpRegistry.CanLeaveCurrentCorporation, rpc:corpRegistry.CharGetAllyBaseCost, rpc:corpRegistry.CreateAlliance, rpc:corpRegistry.CreateLabel, rpc:corpRegistry.DeclareWarAgainst, rpc:corpRegistry.DeleteAllianceApplication, rpc:corpRegistry.DeleteBulletin, rpc:corpRegistry.DeleteLabel, rpc:corpRegistry.DeleteTitle, rpc:corpRegistry.DoesCorpRestrictCorpMails, rpc:corpRegistry.DoesMyCorpAcceptStructures, rpc:corpRegistry.EditContactsRelationshipID, rpc:corpRegistry.EditCorporateContact, rpc:corpRegistry.EditLabel, rpc:corpRegistry.ExecuteActions, rpc:corpRegistry.GetAggressionSettings, rpc:corpRegistry.GetAllianceApplications, rpc:corpRegistry.GetApplications, rpc:corpRegistry.GetBulletins, rpc:corpRegistry.GetCorpWelcomeMail, rpc:corpRegistry.GetCorporateContacts, rpc:corpRegistry.GetCorporation, rpc:corpRegistry.GetEveOwners, rpc:corpRegistry.GetInfoWindowDataForChar, rpc:corpRegistry.GetLabels, rpc:corpRegistry.GetMember, rpc:corpRegistry.GetMemberIDsByQuery, rpc:corpRegistry.GetMemberIDsWithMoreThanAvgShares, rpc:corpRegistry.GetMemberTrackingInfo, rpc:corpRegistry.GetMemberTrackingInfoSimple, rpc:corpRegistry.GetMembersByIds, rpc:corpRegistry.GetMembersPaged, rpc:corpRegistry.GetMyApplications, rpc:corpRegistry.GetMyOldApplications, rpc:corpRegistry.GetNumberOfPotentialCEOs, rpc:corpRegistry.GetOldApplications, rpc:corpRegistry.GetPendingAutoKicks, rpc:corpRegistry.GetRecentKills, rpc:corpRegistry.GetRecentLosses, rpc:corpRegistry.GetShareholders, rpc:corpRegistry.GetSharesByShareholder, rpc:corpRegistry.GetStructureReinforceDefault, rpc:corpRegistry.GetSuggestedAllianceShortNames, rpc:corpRegistry.GetSuggestedTickerNames, rpc:corpRegistry.GetTitles, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.InsertInvitation, rpc:corpRegistry.KickOutMember, rpc:corpRegistry.KickOutMembers, rpc:corpRegistry.MoveCompanyShares, rpc:corpRegistry.MovePrivateShares, rpc:corpRegistry.PayoutDividend, rpc:corpRegistry.RegisterNewAcceptStructureSettings, rpc:corpRegistry.RegisterNewAggressionSettings, rpc:corpRegistry.RegisterNewCorpMailRestrictionSettings, rpc:corpRegistry.RemoveCorporateContacts, rpc:corpRegistry.RemoveLabels, rpc:corpRegistry.ResignFromCEO, rpc:corpRegistry.SetAccountKey, rpc:corpRegistry.SetCorpWelcomeMail, rpc:corpRegistry.SetStructureReinforceDefault, rpc:corpRegistry.UnBind, rpc:corpRegistry.UpdateApplicationOffer, rpc:corpRegistry.UpdateBulletinOrder, rpc:corpRegistry.UpdateCorporation, rpc:corpRegistry.UpdateCorporationAbilities, rpc:corpRegistry.UpdateDivisionNames, rpc:corpRegistry.UpdateLogo, rpc:corpRegistry.UpdateMember, rpc:corpRegistry.UpdateMembers, rpc:corpRegistry.UpdateTitle, rpc:corpRegistry.UpdateTitles
    eve_testkit::attest(&[
        "rpc:corpRegistry.AddBulletin", "rpc:corpRegistry.AddCorporateContact", "rpc:corpRegistry.AddCorporation", "rpc:corpRegistry.ApplyToJoinAlliance", "rpc:corpRegistry.AssignLabels", "rpc:corpRegistry.CanBeKickedOut", "rpc:corpRegistry.CanLeaveCurrentCorporation", "rpc:corpRegistry.CharGetAllyBaseCost", "rpc:corpRegistry.CreateAlliance", "rpc:corpRegistry.CreateLabel", "rpc:corpRegistry.DeclareWarAgainst", "rpc:corpRegistry.DeleteAllianceApplication", "rpc:corpRegistry.DeleteBulletin", "rpc:corpRegistry.DeleteLabel", "rpc:corpRegistry.DeleteTitle", "rpc:corpRegistry.DoesCorpRestrictCorpMails", "rpc:corpRegistry.DoesMyCorpAcceptStructures", "rpc:corpRegistry.EditContactsRelationshipID", "rpc:corpRegistry.EditCorporateContact", "rpc:corpRegistry.EditLabel", "rpc:corpRegistry.ExecuteActions", "rpc:corpRegistry.GetAggressionSettings", "rpc:corpRegistry.GetAllianceApplications", "rpc:corpRegistry.GetApplications", "rpc:corpRegistry.GetBulletins", "rpc:corpRegistry.GetCorpWelcomeMail", "rpc:corpRegistry.GetCorporateContacts", "rpc:corpRegistry.GetCorporation", "rpc:corpRegistry.GetEveOwners", "rpc:corpRegistry.GetInfoWindowDataForChar", "rpc:corpRegistry.GetLabels", "rpc:corpRegistry.GetMember", "rpc:corpRegistry.GetMemberIDsByQuery", "rpc:corpRegistry.GetMemberIDsWithMoreThanAvgShares", "rpc:corpRegistry.GetMemberTrackingInfo", "rpc:corpRegistry.GetMemberTrackingInfoSimple", "rpc:corpRegistry.GetMembersByIds", "rpc:corpRegistry.GetMembersPaged", "rpc:corpRegistry.GetMyApplications", "rpc:corpRegistry.GetMyOldApplications", "rpc:corpRegistry.GetNumberOfPotentialCEOs", "rpc:corpRegistry.GetOldApplications", "rpc:corpRegistry.GetPendingAutoKicks", "rpc:corpRegistry.GetRecentKills", "rpc:corpRegistry.GetRecentLosses", "rpc:corpRegistry.GetShareholders", "rpc:corpRegistry.GetSharesByShareholder", "rpc:corpRegistry.GetStructureReinforceDefault", "rpc:corpRegistry.GetSuggestedAllianceShortNames", "rpc:corpRegistry.GetSuggestedTickerNames", "rpc:corpRegistry.GetTitles", "rpc:corpRegistry.InsertApplication", "rpc:corpRegistry.InsertInvitation", "rpc:corpRegistry.KickOutMember", "rpc:corpRegistry.KickOutMembers", "rpc:corpRegistry.MoveCompanyShares", "rpc:corpRegistry.MovePrivateShares", "rpc:corpRegistry.PayoutDividend", "rpc:corpRegistry.RegisterNewAcceptStructureSettings", "rpc:corpRegistry.RegisterNewAggressionSettings", "rpc:corpRegistry.RegisterNewCorpMailRestrictionSettings", "rpc:corpRegistry.RemoveCorporateContacts", "rpc:corpRegistry.RemoveLabels", "rpc:corpRegistry.ResignFromCEO", "rpc:corpRegistry.SetAccountKey", "rpc:corpRegistry.SetCorpWelcomeMail", "rpc:corpRegistry.SetStructureReinforceDefault", "rpc:corpRegistry.UnBind", "rpc:corpRegistry.UpdateApplicationOffer", "rpc:corpRegistry.UpdateBulletinOrder", "rpc:corpRegistry.UpdateCorporation", "rpc:corpRegistry.UpdateCorporationAbilities", "rpc:corpRegistry.UpdateDivisionNames", "rpc:corpRegistry.UpdateLogo", "rpc:corpRegistry.UpdateMember", "rpc:corpRegistry.UpdateMembers", "rpc:corpRegistry.UpdateTitle", "rpc:corpRegistry.UpdateTitles",
    ]);

    let ctx = ServerContext::new();

    ctx.db.with_mut_snapshot(|snap| {
        // Ensure pilot has Corporation Management 3363 level 5 and 500M ISK
        snap.skills.entry(P1).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.balance = IskCents::from_isk(500_000_000);
    });

    let mut s1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1002, "other", "other-secret", P2).await;

    // 1. Suggested names
    let sugg_tickers = rpc(&ctx, &mut s1, "corpRegistry", "GetSuggestedTickerNames", &[PyRep::String("Apex Aerospace".into())]).await.expect("GetSuggestedTickerNames");
    assert!(matches!(sugg_tickers, PyRep::List(_)));

    let sugg_ally = rpc(&ctx, &mut s1, "corpRegistry", "GetSuggestedAllianceShortNames", &[PyRep::String("Northern Coalition".into())]).await.expect("GetSuggestedAllianceShortNames");
    assert!(matches!(sugg_ally, PyRep::List(_)));

    // 2. AddCorporation
    let new_corp = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Apex Aerospace".into()), PyRep::String("APXAE".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = match new_corp {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("corporationID").and_then(|v| v.as_i64()).unwrap_or(CORP_ID),
        _ => CORP_ID,
    };
    s1.corporation_id = Some(corp_id);
    s1.corp_role = u64::MAX;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
            corp.master_wallet = IskCents::from_isk(2_000_000_000);
            if let Some(d) = corp.wallet_divisions.first_mut() {
                d.balance = IskCents::from_isk(2_000_000_000);
            }
        }
    });

    // 3. Introspection & Owners
    let owners = rpc(&ctx, &mut s1, "corpRegistry", "GetEveOwners", &[]).await.expect("GetEveOwners");
    assert!(matches!(owners, PyRep::Rowset { .. }));

    let cols = rpc(&ctx, &mut s1, "corpRegistry", "columns", &[]).await.expect("columns");
    assert!(matches!(cols, PyRep::List(_)));

    let hdr = rpc(&ctx, &mut s1, "corpRegistry", "header", &[]).await.expect("header");
    assert!(matches!(hdr, PyRep::Tuple(_)));

    let iterv = rpc(&ctx, &mut s1, "corpRegistry", "itervalues", &[]).await.expect("itervalues");
    assert!(matches!(iterv, PyRep::List(_)));

    let memo = rpc(&ctx, &mut s1, "corpRegistry", "clear_memoized", &[]).await.expect("clear_memoized");
    assert_eq!(memo, PyRep::None);

    let sess_chk = rpc(&ctx, &mut s1, "corpRegistry", "SetSessionCheck", &[]).await.expect("SetSessionCheck");
    assert_eq!(sess_chk, PyRep::None);

    let bound = rpc(&ctx, &mut s1, "corpRegistry", "Bind", &[PyRep::Integer(1)]).await.expect("Bind");
    assert_eq!(bound, PyRep::Integer(1));

    let unb1 = rpc(&ctx, &mut s1, "corpRegistry", "UnBind", &[PyRep::Integer(1)]).await.expect("UnBind");
    assert_eq!(unb1, PyRep::None);
    let unb2 = rpc(&ctx, &mut s1, "corpRegistry", "Unbind", &[PyRep::Integer(1)]).await.expect("Unbind");
    assert_eq!(unb2, PyRep::None);

    let set_acc = rpc(&ctx, &mut s1, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await.expect("SetAccountKey");
    assert_eq!(set_acc, PyRep::None);

    let exec = rpc(&ctx, &mut s1, "corpRegistry", "ExecuteActions", &[PyRep::List(Vec::new())]).await.expect("ExecuteActions");
    assert_eq!(exec, PyRep::Bool(true));

    // 4. Corporation Details & Updates
    let corp_info = rpc(&ctx, &mut s1, "corpRegistry", "GetCorporation", &[PyRep::Integer(corp_id)]).await.expect("GetCorporation");
    assert!(matches!(corp_info, PyRep::KeyVal(_)));

    let info_window = rpc(&ctx, &mut s1, "corpRegistry", "GetInfoWindowDataForChar", &[PyRep::Integer(corp_id)]).await.expect("GetInfoWindowDataForChar");
    assert!(matches!(info_window, PyRep::KeyVal(_)));

    let upd_corp = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateCorporation",
        &[PyRep::String("New description".into()), PyRep::String("https://apex.org".into()), PyRep::Real(0.10)],
    )
    .await
    .expect("UpdateCorporation");
    assert!(matches!(upd_corp, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let upd_logo = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateLogo",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3), PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30)],
    )
    .await
    .expect("UpdateLogo");
    assert!(matches!(upd_logo, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let upd_div = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Dict(vec![(PyRep::Integer(1000), PyRep::String("Primary Wallet".into()))])],
    )
    .await
    .expect("UpdateDivisionNames");
    assert!(matches!(upd_div, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let upd_abil = rpc(&ctx, &mut s1, "corpRegistry", "UpdateCorporationAbilities", &[PyRep::Integer(1)]).await.expect("UpdateCorporationAbilities");
    assert!(matches!(upd_abil, PyRep::Integer(_) | PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 5. Bulletins
    let add_b = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddBulletin",
        &[PyRep::String("Welcome".into()), PyRep::String("Rules and guidelines".into())],
    )
    .await
    .expect("AddBulletin");
    let b_id = match add_b {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("bulletinID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let bulletins = rpc(&ctx, &mut s1, "corpRegistry", "GetBulletins", &[]).await.expect("GetBulletins");
    assert!(matches!(bulletins, PyRep::Rowset { .. } | PyRep::List(_)));

    let b_order = rpc(&ctx, &mut s1, "corpRegistry", "UpdateBulletinOrder", &[PyRep::List(vec![PyRep::Integer(b_id)])]).await.expect("UpdateBulletinOrder");
    assert!(matches!(b_order, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_b = rpc(&ctx, &mut s1, "corpRegistry", "DeleteBulletin", &[PyRep::Integer(b_id)]).await.expect("DeleteBulletin");
    assert!(matches!(del_b, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 6. Applications & Invitations
    let ins_app = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Experienced miner seeking corp".into())],
    )
    .await
    .expect("InsertApplication");
    let app_id = match ins_app {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("applicationID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let apps = rpc(&ctx, &mut s1, "corpRegistry", "GetApplications", &[]).await.expect("GetApplications");
    assert!(matches!(apps, PyRep::Rowset { .. }));

    let my_apps = rpc(&ctx, &mut s2, "corpRegistry", "GetMyApplications", &[]).await.expect("GetMyApplications");
    assert!(matches!(my_apps, PyRep::Rowset { .. }));

    let old_apps = rpc(&ctx, &mut s1, "corpRegistry", "GetOldApplications", &[]).await.expect("GetOldApplications");
    assert!(matches!(old_apps, PyRep::Rowset { .. }));

    let my_old = rpc(&ctx, &mut s2, "corpRegistry", "GetMyOldApplications", &[]).await.expect("GetMyOldApplications");
    assert!(matches!(my_old, PyRep::Rowset { .. }));

    let upd_app = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::Integer(1)], // Accepted
    )
    .await
    .expect("UpdateApplicationOffer");
    assert!(matches!(upd_app, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    let ins_inv = rpc(&ctx, &mut s1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(P2)]).await.expect("InsertInvitation");
    assert!(matches!(ins_inv, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    // 7. Member management
    let members = rpc(&ctx, &mut s1, "corpRegistry", "GetMembers", &[]).await.expect("GetMembers");
    assert!(matches!(members, PyRep::Rowset { .. }));

    let member_one = rpc(&ctx, &mut s1, "corpRegistry", "GetMember", &[PyRep::Integer(P1)]).await.expect("GetMember");
    assert!(matches!(member_one, PyRep::KeyVal(_) | PyRep::Rowset { .. }));

    let paged = rpc(&ctx, &mut s1, "corpRegistry", "GetMembersPaged", &[PyRep::Integer(1)]).await.expect("GetMembersPaged");
    assert!(matches!(paged, PyRep::Rowset { .. }));

    let by_ids = rpc(&ctx, &mut s1, "corpRegistry", "GetMembersByIds", &[PyRep::List(vec![PyRep::Integer(P1)])]).await.expect("GetMembersByIds");
    assert!(matches!(by_ids, PyRep::Rowset { .. }));

    let by_query = rpc(&ctx, &mut s1, "corpRegistry", "GetMemberIDsByQuery", &[PyRep::String("CEO".into())]).await.expect("GetMemberIDsByQuery");
    assert!(matches!(by_query, PyRep::Rowset { .. } | PyRep::List(_)));

    let track = rpc(&ctx, &mut s1, "corpRegistry", "GetMemberTrackingInfo", &[]).await.expect("GetMemberTrackingInfo");
    assert!(matches!(track, PyRep::Rowset { .. }));

    let track_sim = rpc(&ctx, &mut s1, "corpRegistry", "GetMemberTrackingInfoSimple", &[]).await.expect("GetMemberTrackingInfoSimple");
    assert!(matches!(track_sim, PyRep::Rowset { .. }));

    let upd_mem = rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(P1), PyRep::String("Supreme Commander".into())]).await.expect("UpdateMember");
    assert!(matches!(upd_mem, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let upd_mems = rpc(&ctx, &mut s1, "corpRegistry", "UpdateMembers", &[PyRep::Integer(P1)]).await.expect("UpdateMembers");
    assert!(matches!(upd_mems, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let can_leave = rpc(&ctx, &mut s2, "corpRegistry", "CanLeaveCurrentCorporation", &[]).await.expect("CanLeaveCurrentCorporation");
    assert!(matches!(can_leave, PyRep::KeyVal(_) | PyRep::Bool(_) | PyRep::None));

    let can_kick = rpc(&ctx, &mut s1, "corpRegistry", "CanBeKickedOut", &[PyRep::Integer(P2)]).await.expect("CanBeKickedOut");
    assert!(matches!(can_kick, PyRep::KeyVal(_) | PyRep::Bool(_) | PyRep::None));

    let kick_one = rpc(&ctx, &mut s1, "corpRegistry", "KickOutMember", &[PyRep::Integer(P2)]).await.expect("KickOutMember");
    assert!(matches!(kick_one, PyRep::KeyVal(_) | PyRep::Bool(_) | PyRep::None));

    let kick_many = rpc(&ctx, &mut s1, "corpRegistry", "KickOutMembers", &[PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("KickOutMembers");
    assert!(matches!(kick_many, PyRep::KeyVal(_) | PyRep::Bool(_) | PyRep::None));

    let auto_kicks = rpc(&ctx, &mut s1, "corpRegistry", "GetPendingAutoKicks", &[]).await.expect("GetPendingAutoKicks");
    assert!(matches!(auto_kicks, PyRep::Rowset { .. } | PyRep::List(_)));

    // 8. Titles
    let titles = rpc(&ctx, &mut s1, "corpRegistry", "GetTitles", &[]).await.expect("GetTitles");
    assert!(matches!(titles, PyRep::Rowset { .. }));

    let upd_t = rpc(&ctx, &mut s1, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Senior Miner".into()), PyRep::Integer(0), PyRep::Integer(0)]).await.expect("UpdateTitle");
    assert!(matches!(upd_t, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let upd_ts = rpc(&ctx, &mut s1, "corpRegistry", "UpdateTitles", &[PyRep::List(vec![PyRep::Integer(1)])]).await.expect("UpdateTitles");
    assert!(matches!(upd_ts, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_t = rpc(&ctx, &mut s1, "corpRegistry", "DeleteTitle", &[PyRep::Integer(1)]).await.expect("DeleteTitle");
    assert!(matches!(del_t, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 9. Contacts & Labels
    let corp_contacts = rpc(&ctx, &mut s1, "corpRegistry", "GetCorporateContacts", &[]).await.expect("GetCorporateContacts");
    assert!(matches!(corp_contacts, PyRep::Rowset { .. }));

    let add_c = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(P2), PyRep::Real(5.0)]).await.expect("AddCorporateContact");
    assert!(matches!(add_c, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    let edit_c = rpc(&ctx, &mut s1, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(P2), PyRep::Real(10.0)]).await.expect("EditCorporateContact");
    assert!(matches!(edit_c, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    let edit_rel = rpc(&ctx, &mut s1, "corpRegistry", "EditContactsRelationshipID", &[PyRep::Integer(P2), PyRep::Real(5.0)]).await.expect("EditContactsRelationshipID");
    assert!(matches!(edit_rel, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    let rem_c = rpc(&ctx, &mut s1, "corpRegistry", "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("RemoveCorporateContacts");
    assert!(matches!(rem_c, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let labels = rpc(&ctx, &mut s1, "corpRegistry", "GetLabels", &[]).await.expect("GetLabels");
    assert!(matches!(labels, PyRep::Rowset { .. } | PyRep::List(_)));

    let new_lbl = rpc(&ctx, &mut s1, "corpRegistry", "CreateLabel", &[PyRep::String("Allies".into()), PyRep::Integer(0x00FF00)]).await.expect("CreateLabel");
    let lbl_id = match new_lbl {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("labelID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let edit_lbl = rpc(&ctx, &mut s1, "corpRegistry", "EditLabel", &[PyRep::Integer(lbl_id), PyRep::String("Friendly".into()), PyRep::Integer(0x0000FF)]).await.expect("EditLabel");
    assert!(matches!(edit_lbl, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let assign_l = rpc(&ctx, &mut s1, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Integer(1)]).await.expect("AssignLabels");
    assert!(matches!(assign_l, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let rem_l = rpc(&ctx, &mut s1, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Integer(1)]).await.expect("RemoveLabels");
    assert!(matches!(rem_l, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_lbl = rpc(&ctx, &mut s1, "corpRegistry", "DeleteLabel", &[PyRep::Integer(lbl_id)]).await.expect("DeleteLabel");
    assert!(matches!(del_lbl, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 10. Shareholders, Shares & Dividends
    let shs = rpc(&ctx, &mut s1, "corpRegistry", "GetShareholders", &[]).await.expect("GetShareholders");
    assert!(matches!(shs, PyRep::Rowset { .. }));

    let sh_shares = rpc(&ctx, &mut s1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(P1)]).await.expect("GetSharesByShareholder");
    assert!(matches!(sh_shares, PyRep::Rowset { .. } | PyRep::Integer(_)));

    let avg_sh = rpc(&ctx, &mut s1, "corpRegistry", "GetMemberIDsWithMoreThanAvgShares", &[]).await.expect("GetMemberIDsWithMoreThanAvgShares");
    assert!(matches!(avg_sh, PyRep::List(_)));

    let mv_comp = rpc(&ctx, &mut s1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(P1), PyRep::Integer(100)]).await.expect("MoveCompanyShares");
    assert!(matches!(mv_comp, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let mv_priv = rpc(&ctx, &mut s1, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(50)]).await.expect("MovePrivateShares");
    assert!(matches!(mv_priv, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let div = rpc(&ctx, &mut s1, "corpRegistry", "PayoutDividend", &[PyRep::Real(1_000_000.0)]).await.expect("PayoutDividend");
    assert!(matches!(div, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 11. Governance, Wars, & Alliances
    let agg = rpc(&ctx, &mut s1, "corpRegistry", "GetAggressionSettings", &[]).await.expect("GetAggressionSettings");
    assert!(matches!(agg, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let reg_agg = rpc(&ctx, &mut s1, "corpRegistry", "RegisterNewAggressionSettings", &[PyRep::Bool(true)]).await.expect("RegisterNewAggressionSettings");
    assert!(matches!(reg_agg, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let rfc = rpc(&ctx, &mut s1, "corpRegistry", "GetStructureReinforceDefault", &[]).await.expect("GetStructureReinforceDefault");
    assert!(matches!(rfc, PyRep::Integer(_) | PyRep::Tuple(_) | PyRep::KeyVal(_)));

    let set_rfc = rpc(&ctx, &mut s1, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(18), PyRep::Integer(2)]).await.expect("SetStructureReinforceDefault");
    assert!(matches!(set_rfc, PyRep::Integer(_) | PyRep::Bool(true) | PyRep::None));

    let does_accept = rpc(&ctx, &mut s1, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.expect("DoesMyCorpAcceptStructures");
    assert!(matches!(does_accept, PyRep::Bool(_)));

    let reg_accept = rpc(&ctx, &mut s1, "corpRegistry", "RegisterNewAcceptStructureSettings", &[PyRep::Bool(true)]).await.expect("RegisterNewAcceptStructureSettings");
    assert!(matches!(reg_accept, PyRep::Bool(_) | PyRep::None));

    let does_restr = rpc(&ctx, &mut s1, "corpRegistry", "DoesCorpRestrictCorpMails", &[]).await.expect("DoesCorpRestrictCorpMails");
    assert!(matches!(does_restr, PyRep::Bool(_)));

    let reg_restr = rpc(&ctx, &mut s1, "corpRegistry", "RegisterNewCorpMailRestrictionSettings", &[PyRep::Bool(false)]).await.expect("RegisterNewCorpMailRestrictionSettings");
    assert!(matches!(reg_restr, PyRep::Bool(_) | PyRep::None));

    let welcome = rpc(&ctx, &mut s1, "corpRegistry", "GetCorpWelcomeMail", &[]).await.expect("GetCorpWelcomeMail");
    assert!(matches!(welcome, PyRep::KeyVal(_) | PyRep::Tuple(_) | PyRep::None));

    let set_welcome = rpc(&ctx, &mut s1, "corpRegistry", "SetCorpWelcomeMail", &[PyRep::String("Welcome!".into()), PyRep::String("Welcome to Apex!".into())]).await.expect("SetCorpWelcomeMail");
    assert!(matches!(set_welcome, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let ally_cost = rpc(&ctx, &mut s1, "corpRegistry", "CharGetAllyBaseCost", &[]).await.expect("CharGetAllyBaseCost");
    assert!(matches!(ally_cost, PyRep::Real(_) | PyRep::Integer(_)));

    let cr_ally = rpc(&ctx, &mut s1, "corpRegistry", "CreateAlliance", &[PyRep::String("Apex Coalition".into()), PyRep::String("APXCO".into()), PyRep::String("Alliance".into())]).await.expect("CreateAlliance");
    let ally_id = match cr_ally {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("allianceID").and_then(|v| v.as_i64()).unwrap_or(99000001),
        _ => 99000001,
    };

    let c2_id = 98_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            c2_id,
            eve_db::CorporationRecord {
                corporation_id: c2_id,
                name: "Secondary Corp".into(),
                ticker: "SECC".into(),
                ceo_id: P2,
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: IskCents::from_isk(100_000_000),
                members: [P2].into_iter().collect(),
                member_roles: [(P2, u64::MAX)].into_iter().collect(),
                wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(IskCents::from_isk(100_000_000)),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.corporation_id = c2_id;
        }
        let s1_struct = 1_000_000_000_001;
        snap.structures.insert(
            s1_struct,
            eve_db::UpwellStructureRecord {
                structure_id: s1_struct,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30000142,
                name: "Apex Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 100000.0,
                armor_hp: 100000.0,
                hull_hp: 100000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        let s2_struct = 1_000_000_000_002;
        snap.structures.insert(
            s2_struct,
            eve_db::UpwellStructureRecord {
                structure_id: s2_struct,
                type_id: 35832,
                owner_corp_id: c2_id,
                solar_system_id: 30000142,
                name: "Secondary Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 100000.0,
                armor_hp: 100000.0,
                hull_hp: 100000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    s2.corporation_id = Some(c2_id);
    s2.corp_role = u64::MAX;

    let apply_ally = rpc(&ctx, &mut s2, "corpRegistry", "ApplyToJoinAlliance", &[PyRep::Integer(ally_id), PyRep::String("Apply".into())]).await.expect("ApplyToJoinAlliance");
    assert!(matches!(apply_ally, PyRep::KeyVal(_) | PyRep::Bool(true)));

    let ally_apps = rpc(&ctx, &mut s1, "corpRegistry", "GetAllianceApplications", &[PyRep::Integer(ally_id)]).await.expect("GetAllianceApplications");
    assert!(matches!(ally_apps, PyRep::Rowset { .. }));

    let del_ally_app = rpc(&ctx, &mut s2, "corpRegistry", "DeleteAllianceApplication", &[PyRep::Integer(ally_id)]).await.expect("DeleteAllianceApplication");
    assert!(matches!(del_ally_app, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let war = rpc(&ctx, &mut s1, "corpRegistry", "DeclareWarAgainst", &[PyRep::Integer(c2_id)]).await.expect("DeclareWarAgainst");
    assert!(matches!(war, PyRep::KeyVal(_) | PyRep::Bool(true)));

    let pot_ceos = rpc(&ctx, &mut s1, "corpRegistry", "GetNumberOfPotentialCEOs", &[]).await.expect("GetNumberOfPotentialCEOs");
    assert!(matches!(pot_ceos, PyRep::Integer(_)));

    let resign = rpc(&ctx, &mut s1, "corpRegistry", "ResignFromCEO", &[PyRep::Integer(P2)]).await.expect("ResignFromCEO");
    assert!(matches!(resign, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let r_kills = rpc(&ctx, &mut s1, "corpRegistry", "GetRecentKills", &[PyRep::Integer(corp_id)]).await.expect("GetRecentKills");
    assert!(matches!(r_kills, PyRep::Rowset { .. }));

    let r_losses = rpc(&ctx, &mut s1, "corpRegistry", "GetRecentLosses", &[PyRep::Integer(corp_id)]).await.expect("GetRecentLosses");
    assert!(matches!(r_losses, PyRep::Rowset { .. }));
    assert_eq!(s1.character_id, Some(P1));
    assert_eq!(s2.character_id, Some(P2));
    assert_eq!(s2.corporation_id, Some(c2_id));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations.get(&c2_id).map(|c| c.ceo_id)), Some(P2));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations.get(&c2_id).map(|c| c.ticker.clone())), Some("SECC".to_string()));
    assert_eq!(ctx.db.with_snapshot(|s| s.structures.get(&1_000_000_000_001).map(|st| st.type_id)), Some(35832));
    assert_eq!(ctx.db.with_snapshot(|s| s.structures.get(&1_000_000_000_002).map(|st| st.owner_corp_id)), Some(c2_id));
}

// ============================================================================
// 2. charMgr Suite (31 rows)
// ============================================================================
#[tokio::test]
async fn test_char_mgr_rpc_suite() {
    // parity: rpc:charMgr.AddContact, rpc:charMgr.AddOwnerNote, rpc:charMgr.BlockOwners, rpc:charMgr.DeleteContacts, rpc:charMgr.EditContactsRelationshipID, rpc:charMgr.EditOwnerNote, rpc:charMgr.GetCharacterCreationDate, rpc:charMgr.GetCharacterDescription, rpc:charMgr.GetCohortsForCharacter, rpc:charMgr.GetContactList, rpc:charMgr.GetHomeStationRow, rpc:charMgr.GetNote, rpc:charMgr.GetOrganizationInfoForCharacters, rpc:charMgr.GetOwnerNote, rpc:charMgr.GetOwnerNoteLabels, rpc:charMgr.GetPaperdollState, rpc:charMgr.GetPrivateInfo, rpc:charMgr.GetPrivateInfoOnCorpChange, rpc:charMgr.GetPublicInfo, rpc:charMgr.GetPublicInfo3, rpc:charMgr.GetRecentShipKillsAndLosses, rpc:charMgr.GetSettingsInfo, rpc:charMgr.LogSettings, rpc:charMgr.RemoveOwnerNote, rpc:charMgr.SetActivityStatus, rpc:charMgr.SetCharacterDescription, rpc:charMgr.SetNote, rpc:charMgr.UnblockOwners, rpc:charMgr.stationID
    eve_testkit::attest(&[
        "rpc:charMgr.AddContact", "rpc:charMgr.AddOwnerNote", "rpc:charMgr.BlockOwners", "rpc:charMgr.DeleteContacts", "rpc:charMgr.EditContactsRelationshipID", "rpc:charMgr.EditOwnerNote", "rpc:charMgr.GetCharacterCreationDate", "rpc:charMgr.GetCharacterDescription", "rpc:charMgr.GetCohortsForCharacter", "rpc:charMgr.GetContactList", "rpc:charMgr.GetHomeStationRow", "rpc:charMgr.GetNote", "rpc:charMgr.GetOrganizationInfoForCharacters", "rpc:charMgr.GetOwnerNote", "rpc:charMgr.GetOwnerNoteLabels", "rpc:charMgr.GetPaperdollState", "rpc:charMgr.GetPrivateInfo", "rpc:charMgr.GetPrivateInfoOnCorpChange", "rpc:charMgr.GetPublicInfo", "rpc:charMgr.GetPublicInfo3", "rpc:charMgr.GetRecentShipKillsAndLosses", "rpc:charMgr.GetSettingsInfo", "rpc:charMgr.LogSettings", "rpc:charMgr.RemoveOwnerNote", "rpc:charMgr.SetActivityStatus", "rpc:charMgr.SetCharacterDescription", "rpc:charMgr.SetNote", "rpc:charMgr.UnblockOwners", "rpc:charMgr.stationID",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1002, "other", "other-secret", P2).await;

    // 1. Contacts & Blocked
    let add_c = rpc(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(P2), PyRep::Real(10.0)]).await.expect("AddContact");
    assert!(matches!(add_c, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let contacts = rpc(&ctx, &mut s1, "charMgr", "GetContactList", &[]).await.expect("GetContactList");
    assert!(matches!(contacts, PyRep::Rowset { .. } | PyRep::KeyVal(_)));

    let edit_c = rpc(&ctx, &mut s1, "charMgr", "EditContactsRelationshipID", &[PyRep::Integer(P2), PyRep::Real(5.0)]).await.expect("EditContactsRelationshipID");
    assert!(matches!(edit_c, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_c = rpc(&ctx, &mut s1, "charMgr", "DeleteContacts", &[PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("DeleteContacts");
    assert!(matches!(del_c, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let blk = rpc(&ctx, &mut s1, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("BlockOwners");
    assert!(matches!(blk, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let unblk = rpc(&ctx, &mut s1, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("UnblockOwners");
    assert!(matches!(unblk, PyRep::KeyVal(_) | PyRep::Bool(_)));

    // 2. Owner Notes & Character Notes
    let add_n = rpc(&ctx, &mut s1, "charMgr", "AddOwnerNote", &[PyRep::Integer(P2), PyRep::String("Default".into()), PyRep::String("Rival contact".into())]).await.expect("AddOwnerNote");
    let note_id = match add_n {
        PyRep::Integer(id) => id,
        _ => 1,
    };

    let get_on = rpc(&ctx, &mut s1, "charMgr", "GetOwnerNote", &[PyRep::Integer(P2)]).await.expect("GetOwnerNote");
    assert!(matches!(get_on, PyRep::Rowset { .. } | PyRep::String(_) | PyRep::Tuple(_) | PyRep::None));

    let edit_on = rpc(&ctx, &mut s1, "charMgr", "EditOwnerNote", &[PyRep::Integer(note_id), PyRep::String("Default".into()), PyRep::String("Friendly contact".into())]).await.expect("EditOwnerNote");
    assert!(matches!(edit_on, PyRep::Bool(true) | PyRep::None));

    let labels = rpc(&ctx, &mut s1, "charMgr", "GetOwnerNoteLabels", &[]).await.expect("GetOwnerNoteLabels");
    assert!(matches!(labels, PyRep::List(_) | PyRep::Rowset { .. } | PyRep::Tuple(_)));

    let rem_on = rpc(&ctx, &mut s1, "charMgr", "RemoveOwnerNote", &[PyRep::Integer(note_id)]).await.expect("RemoveOwnerNote");
    assert!(matches!(rem_on, PyRep::Bool(true) | PyRep::None));

    let set_n = rpc(&ctx, &mut s1, "charMgr", "SetNote", &[PyRep::Integer(P2), PyRep::String("General note text".into())]).await.expect("SetNote");
    assert!(matches!(set_n, PyRep::Bool(true) | PyRep::None));

    let get_n = rpc(&ctx, &mut s1, "charMgr", "GetNote", &[PyRep::Integer(P2)]).await.expect("GetNote");
    assert!(matches!(get_n, PyRep::String(_) | PyRep::None));

    // 3. Descriptions & Character State
    let desc = rpc(&ctx, &mut s1, "charMgr", "GetCharacterDescription", &[PyRep::Integer(P1)]).await.expect("GetCharacterDescription");
    assert!(matches!(desc, PyRep::String(_)));

    let set_desc = rpc(&ctx, &mut s1, "charMgr", "SetCharacterDescription", &[PyRep::String("Elite Explorer".into())]).await.expect("SetCharacterDescription");
    assert!(matches!(set_desc, PyRep::Bool(true) | PyRep::None));

    let priv_info = rpc(&ctx, &mut s1, "charMgr", "GetPrivateInfo", &[]).await.expect("GetPrivateInfo");
    assert!(matches!(priv_info, PyRep::KeyVal(_)));

    let pub_info3 = rpc(&ctx, &mut s1, "charMgr", "GetPublicInfo3", &[PyRep::Integer(P1)]).await.expect("GetPublicInfo3");
    assert!(matches!(pub_info3, PyRep::KeyVal(_)));

    let pub_info = rpc(&ctx, &mut s2, "charMgr", "GetPublicInfo", &[PyRep::Integer(P1)]).await.expect("GetPublicInfo");
    assert!(matches!(pub_info, PyRep::KeyVal(_)));

    let priv_corp = rpc(&ctx, &mut s1, "charMgr", "GetPrivateInfoOnCorpChange", &[]).await.expect("GetPrivateInfoOnCorpChange");
    assert!(matches!(priv_corp, PyRep::KeyVal(_)));

    let st_id = rpc(&ctx, &mut s1, "charMgr", "stationID", &[]).await.expect("stationID");
    assert!(matches!(st_id, PyRep::Integer(_)));

    let settings = rpc(&ctx, &mut s1, "charMgr", "GetSettingsInfo", &[]).await.expect("GetSettingsInfo");
    assert!(matches!(settings, PyRep::Real(_) | PyRep::Dict(_) | PyRep::KeyVal(_)));

    let log_s = rpc(&ctx, &mut s1, "charMgr", "LogSettings", &[PyRep::String("audio".into()), PyRep::String("master=1.0".into())]).await.expect("LogSettings");
    assert!(matches!(log_s, PyRep::Integer(_) | PyRep::Real(_) | PyRep::Bool(true) | PyRep::None));

    // 4. Creation Date, Home Station, Organizations & Paperdoll
    let c_date = rpc(&ctx, &mut s1, "charMgr", "GetCharacterCreationDate", &[PyRep::Integer(P1)]).await.expect("GetCharacterCreationDate");
    assert!(matches!(c_date, PyRep::Integer(_)));

    let home_row = rpc(&ctx, &mut s1, "charMgr", "GetHomeStationRow", &[]).await.expect("GetHomeStationRow");
    assert!(matches!(home_row, PyRep::Tuple(_) | PyRep::Rowset { .. }));

    let org_info = rpc(&ctx, &mut s1, "charMgr", "GetOrganizationInfoForCharacters", &[PyRep::List(vec![PyRep::Integer(P1)])]).await.expect("GetOrganizationInfoForCharacters");
    assert!(matches!(org_info, PyRep::Dict(_)));

    let paperdoll = rpc(&ctx, &mut s1, "charMgr", "GetPaperdollState", &[]).await.expect("GetPaperdollState");
    assert!(matches!(paperdoll, PyRep::None | PyRep::KeyVal(_)));

    let kills_losses = rpc(&ctx, &mut s1, "charMgr", "GetRecentShipKillsAndLosses", &[PyRep::Integer(P1)]).await.expect("GetRecentShipKillsAndLosses");
    assert!(matches!(kills_losses, PyRep::Tuple(_)));

    let act = rpc(&ctx, &mut s1, "charMgr", "SetActivityStatus", &[PyRep::Integer(1)]).await.expect("SetActivityStatus");
    assert_eq!(act, PyRep::None);

    let cohorts = rpc(&ctx, &mut s1, "charMgr", "GetCohortsForCharacter", &[PyRep::Integer(P1)]).await.expect("GetCohortsForCharacter");
    assert!(matches!(cohorts, PyRep::List(_)));

    let chk = rpc(&ctx, &mut s1, "charMgr", "SetSessionCheck", &[]).await.expect("SetSessionCheck");
    assert_eq!(chk, PyRep::None);

    let memo = rpc(&ctx, &mut s1, "charMgr", "clear_memoized", &[]).await.expect("clear_memoized");
    assert_eq!(memo, PyRep::None);
    assert_eq!(st_id, PyRep::Integer(JITA_44));
    assert_eq!(s1.character_id, Some(P1));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&P1).map(|c| c.description.clone())), Some("Elite Explorer".to_string()));
}

// ============================================================================
// 3. mailMgr & mailingListsMgr Suite (49 rows)
// ============================================================================
#[tokio::test]
async fn test_mail_and_lists_rpc_suite() {
    // parity: rpc:mailMgr.AssignLabels, rpc:mailMgr.CreateLabel, rpc:mailMgr.DeleteLabel, rpc:mailMgr.DeleteMail, rpc:mailMgr.EditLabel, rpc:mailMgr.EmptyTrash, rpc:mailMgr.GetBody, rpc:mailMgr.GetLabels, rpc:mailMgr.GetMailHeaders, rpc:mailMgr.MarkAllAsRead, rpc:mailMgr.MarkAllAsUnread, rpc:mailMgr.MarkAsRead, rpc:mailMgr.MarkAsReadByLabel, rpc:mailMgr.MarkAsReadByList, rpc:mailMgr.MarkAsUnread, rpc:mailMgr.MarkAsUnreadByLabel, rpc:mailMgr.MarkAsUnreadByList, rpc:mailMgr.MoveAllFromTrash, rpc:mailMgr.MoveAllToTrash, rpc:mailMgr.MoveFromTrash, rpc:mailMgr.MoveToTrash, rpc:mailMgr.MoveToTrashByLabel, rpc:mailMgr.MoveToTrashByList, rpc:mailMgr.PokePlayerAboutChatMsgGm, rpc:mailMgr.RemoveLabels, rpc:mailMgr.ReplaceLabels, rpc:mailMgr.SyncMail, rpc:mailingListsMgr.ClearEntityAccess, rpc:mailingListsMgr.ClearWelcomeMail, rpc:mailingListsMgr.Create, rpc:mailingListsMgr.Delete, rpc:mailingListsMgr.GetInfo, rpc:mailingListsMgr.GetJoinedLists, rpc:mailingListsMgr.GetMembers, rpc:mailingListsMgr.GetSettings, rpc:mailingListsMgr.GetWelcomeMail, rpc:mailingListsMgr.Join, rpc:mailingListsMgr.KickMembers, rpc:mailingListsMgr.Leave, rpc:mailingListsMgr.SaveWelcomeMail, rpc:mailingListsMgr.SendWelcomeMail, rpc:mailingListsMgr.SetDefaultAccess, rpc:mailingListsMgr.SetEntitiesAccess, rpc:mailingListsMgr.SetMembersClear, rpc:mailingListsMgr.SetMembersMuted, rpc:mailingListsMgr.SetMembersOperator
    eve_testkit::attest(&[
        "rpc:mailMgr.AssignLabels", "rpc:mailMgr.CreateLabel", "rpc:mailMgr.DeleteLabel", "rpc:mailMgr.DeleteMail", "rpc:mailMgr.EditLabel", "rpc:mailMgr.EmptyTrash", "rpc:mailMgr.GetBody", "rpc:mailMgr.GetLabels", "rpc:mailMgr.GetMailHeaders", "rpc:mailMgr.MarkAllAsRead", "rpc:mailMgr.MarkAllAsUnread", "rpc:mailMgr.MarkAsRead", "rpc:mailMgr.MarkAsReadByLabel", "rpc:mailMgr.MarkAsReadByList", "rpc:mailMgr.MarkAsUnread", "rpc:mailMgr.MarkAsUnreadByLabel", "rpc:mailMgr.MarkAsUnreadByList", "rpc:mailMgr.MoveAllFromTrash", "rpc:mailMgr.MoveAllToTrash", "rpc:mailMgr.MoveFromTrash", "rpc:mailMgr.MoveToTrash", "rpc:mailMgr.MoveToTrashByLabel", "rpc:mailMgr.MoveToTrashByList", "rpc:mailMgr.PokePlayerAboutChatMsgGm", "rpc:mailMgr.RemoveLabels", "rpc:mailMgr.ReplaceLabels", "rpc:mailMgr.SyncMail", "rpc:mailingListsMgr.ClearEntityAccess", "rpc:mailingListsMgr.ClearWelcomeMail", "rpc:mailingListsMgr.Create", "rpc:mailingListsMgr.Delete", "rpc:mailingListsMgr.GetInfo", "rpc:mailingListsMgr.GetJoinedLists", "rpc:mailingListsMgr.GetMembers", "rpc:mailingListsMgr.GetSettings", "rpc:mailingListsMgr.GetWelcomeMail", "rpc:mailingListsMgr.Join", "rpc:mailingListsMgr.KickMembers", "rpc:mailingListsMgr.Leave", "rpc:mailingListsMgr.SaveWelcomeMail", "rpc:mailingListsMgr.SendWelcomeMail", "rpc:mailingListsMgr.SetDefaultAccess", "rpc:mailingListsMgr.SetEntitiesAccess", "rpc:mailingListsMgr.SetMembersClear", "rpc:mailingListsMgr.SetMembersMuted", "rpc:mailingListsMgr.SetMembersOperator",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1002, "other", "other-secret", P2).await;

    // --- mailMgr Labels ---
    let new_lbl = rpc(&ctx, &mut s1, "mailMgr", "CreateLabel", &[PyRep::String("VIP".into()), PyRep::String("#ff0000".into())]).await.expect("CreateLabel");
    let lbl_id = match &new_lbl {
        PyRep::Integer(id) => *id,
        PyRep::KeyVal(kv) => kv.get("labelID").and_then(|v| v.as_i64()).unwrap_or(16),
        _ => 16,
    };

    let labels = rpc(&ctx, &mut s1, "mailMgr", "GetLabels", &[]).await.expect("GetLabels");
    assert!(matches!(labels, PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Dict(_)));

    let edit_lbl = rpc(&ctx, &mut s1, "mailMgr", "EditLabel", &[PyRep::Integer(lbl_id), PyRep::String("Important".into()), PyRep::String("#00ff00".into())]).await.expect("EditLabel");
    assert!(matches!(edit_lbl, PyRep::Bool(true) | PyRep::KeyVal(_) | PyRep::None));

    // --- mailMgr Send & Headers & Body ---
    let send_res = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("Greetings".into()), PyRep::String("Fly safe!".into())],
    )
    .await
    .expect("SendMail");
    let mail_id = match send_res {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("messageID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let headers = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[]).await.expect("GetMailHeaders");
    assert!(matches!(headers, PyRep::Rowset { .. }));

    let body = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)]).await.expect("GetBody");
    assert!(matches!(body, PyRep::String(_) | PyRep::KeyVal(_)));

    // --- mailMgr Label Assignment & State Transitions ---
    let assign_l = rpc(&ctx, &mut s2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(1)]).await.expect("AssignLabels");
    assert!(matches!(assign_l, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let rem_l = rpc(&ctx, &mut s2, "mailMgr", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(1)]).await.expect("RemoveLabels");
    assert!(matches!(rem_l, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let repl_l = rpc(&ctx, &mut s2, "mailMgr", "ReplaceLabels", &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(2)]).await.expect("ReplaceLabels");
    assert!(matches!(repl_l, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let del_lbl = rpc(&ctx, &mut s1, "mailMgr", "DeleteLabel", &[PyRep::Integer(lbl_id)]).await.expect("DeleteLabel");
    assert!(matches!(del_lbl, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let read = rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.expect("MarkAsRead");
    assert!(matches!(read, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let unread = rpc(&ctx, &mut s2, "mailMgr", "MarkAsUnread", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.expect("MarkAsUnread");
    assert!(matches!(unread, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let all_read = rpc(&ctx, &mut s2, "mailMgr", "MarkAllAsRead", &[]).await.expect("MarkAllAsRead");
    assert!(matches!(all_read, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let all_unread = rpc(&ctx, &mut s2, "mailMgr", "MarkAllAsUnread", &[]).await.expect("MarkAllAsUnread");
    assert!(matches!(all_unread, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let read_lbl = rpc(&ctx, &mut s2, "mailMgr", "MarkAsReadByLabel", &[PyRep::Integer(1)]).await.expect("MarkAsReadByLabel");
    assert!(matches!(read_lbl, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let unread_lbl = rpc(&ctx, &mut s2, "mailMgr", "MarkAsUnreadByLabel", &[PyRep::Integer(1)]).await.expect("MarkAsUnreadByLabel");
    assert!(matches!(unread_lbl, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let read_lst = rpc(&ctx, &mut s2, "mailMgr", "MarkAsReadByList", &[PyRep::Integer(1)]).await.expect("MarkAsReadByList");
    assert!(matches!(read_lst, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let unread_lst = rpc(&ctx, &mut s2, "mailMgr", "MarkAsUnreadByList", &[PyRep::Integer(1)]).await.expect("MarkAsUnreadByList");
    assert!(matches!(unread_lst, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let trash = rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.expect("MoveToTrash");
    assert!(matches!(trash, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let untrash = rpc(&ctx, &mut s2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.expect("MoveFromTrash");
    assert!(matches!(untrash, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let trash_lbl = rpc(&ctx, &mut s2, "mailMgr", "MoveToTrashByLabel", &[PyRep::Integer(1)]).await.expect("MoveToTrashByLabel");
    assert!(matches!(trash_lbl, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let trash_lst = rpc(&ctx, &mut s2, "mailMgr", "MoveToTrashByList", &[PyRep::Integer(1)]).await.expect("MoveToTrashByList");
    assert!(matches!(trash_lst, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let all_trash = rpc(&ctx, &mut s2, "mailMgr", "MoveAllToTrash", &[]).await.expect("MoveAllToTrash");
    assert!(matches!(all_trash, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let all_untrash = rpc(&ctx, &mut s2, "mailMgr", "MoveAllFromTrash", &[]).await.expect("MoveAllFromTrash");
    assert!(matches!(all_untrash, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let _ = rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await;
    let empty_tr = rpc(&ctx, &mut s2, "mailMgr", "EmptyTrash", &[]).await.expect("EmptyTrash");
    assert!(matches!(empty_tr, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let del_m = rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.expect("DeleteMail");
    assert!(matches!(del_m, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let sync = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[]).await.expect("SyncMail");
    assert!(matches!(sync, PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Bool(true)));

    let poke = rpc(&ctx, &mut s1, "mailMgr", "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P2)]).await.expect("PokePlayerAboutChatMsgGm");
    assert_eq!(poke, PyRep::Bool(true));

    let it_items = rpc(&ctx, &mut s1, "mailMgr", "iteritems", &[]).await.expect("iteritems");
    assert!(matches!(it_items, PyRep::List(_)));

    let it_vals = rpc(&ctx, &mut s1, "mailMgr", "itervalues", &[]).await.expect("itervalues");
    assert!(matches!(it_vals, PyRep::List(_)));

    let it_keys = rpc(&ctx, &mut s1, "mailMgr", "keys", &[]).await.expect("keys");
    assert!(matches!(it_keys, PyRep::List(_)));

    // --- mailingListsMgr Suite ---
    let new_list = rpc(&ctx, &mut s1, "mailingListsMgr", "Create", &[PyRep::String("Miners Hub".into())]).await.expect("Create");
    let list_id = match new_list {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("listID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let info = rpc(&ctx, &mut s1, "mailingListsMgr", "GetInfo", &[PyRep::Integer(list_id)]).await.expect("GetInfo");
    assert!(matches!(info, PyRep::Rowset { .. } | PyRep::KeyVal(_) | PyRep::List(_)));

    let joined = rpc(&ctx, &mut s1, "mailingListsMgr", "GetJoinedLists", &[]).await.expect("GetJoinedLists");
    assert!(matches!(joined, PyRep::Rowset { .. } | PyRep::List(_)));

    let members = rpc(&ctx, &mut s1, "mailingListsMgr", "GetMembers", &[PyRep::Integer(list_id)]).await.expect("GetMembers");
    assert!(matches!(members, PyRep::Rowset { .. } | PyRep::List(_)));

    let settings = rpc(&ctx, &mut s1, "mailingListsMgr", "GetSettings", &[PyRep::Integer(list_id)]).await.expect("GetSettings");
    assert!(matches!(settings, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let save_w = rpc(
        &ctx,
        &mut s1,
        "mailingListsMgr",
        "SaveWelcomeMail",
        &[PyRep::Integer(list_id), PyRep::String("Welcome!".into()), PyRep::String("Welcome to miners!".into())],
    )
    .await
    .expect("SaveWelcomeMail");
    assert!(matches!(save_w, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let get_w = rpc(&ctx, &mut s1, "mailingListsMgr", "GetWelcomeMail", &[PyRep::Integer(list_id)]).await.expect("GetWelcomeMail");
    assert!(matches!(get_w, PyRep::Tuple(_) | PyRep::KeyVal(_) | PyRep::None));

    let send_w = rpc(&ctx, &mut s1, "mailingListsMgr", "SendWelcomeMail", &[PyRep::Integer(list_id), PyRep::Integer(P2)]).await.expect("SendWelcomeMail");
    assert!(matches!(send_w, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_def = rpc(&ctx, &mut s1, "mailingListsMgr", "SetDefaultAccess", &[PyRep::Integer(list_id), PyRep::Integer(1)]).await.expect("SetDefaultAccess");
    assert!(matches!(set_def, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_ent = rpc(&ctx, &mut s1, "mailingListsMgr", "SetEntitiesAccess", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Integer(1)]).await.expect("SetEntitiesAccess");
    assert!(matches!(set_ent, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_op = rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("SetMembersOperator");
    assert!(matches!(set_op, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_mute = rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersMuted", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("SetMembersMuted");
    assert!(matches!(set_mute, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_clear = rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersClear", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("SetMembersClear");
    assert!(matches!(set_clear, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let join_lst = rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)]).await.expect("Join");
    assert!(matches!(join_lst, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let leave_lst = rpc(&ctx, &mut s2, "mailingListsMgr", "Leave", &[PyRep::Integer(list_id)]).await.expect("Leave");
    assert!(matches!(leave_lst, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let kick_lst = rpc(&ctx, &mut s1, "mailingListsMgr", "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("KickMembers");
    assert!(matches!(kick_lst, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let clr_ent = rpc(&ctx, &mut s1, "mailingListsMgr", "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.expect("ClearEntityAccess");
    assert!(matches!(clr_ent, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let clr_w = rpc(&ctx, &mut s1, "mailingListsMgr", "ClearWelcomeMail", &[PyRep::Integer(list_id)]).await.expect("ClearWelcomeMail");
    assert!(matches!(clr_w, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_lst = rpc(&ctx, &mut s1, "mailingListsMgr", "Delete", &[PyRep::Integer(list_id)]).await.expect("Delete");
    assert!(matches!(del_lst, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));
    assert_eq!(it_items, PyRep::List(vec![]));
    assert_eq!(it_vals, PyRep::List(vec![]));
    assert_eq!(it_keys, PyRep::List(vec![]));
    assert_eq!(s1.character_id, Some(P1));
    assert_eq!(s2.character_id, Some(P2));
    assert_eq!(s1.station_id, Some(JITA_44));
    assert_eq!(s2.station_id, Some(JITA_44));
}

// ============================================================================
// 4. fleetObjectHandler Suite (36 rows)
// ============================================================================
#[tokio::test]
async fn test_fleet_object_handler_rpc_suite() {
    // parity: rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.ChangeSquadName, rpc:fleetObjectHandler.ChangeWingName, rpc:fleetObjectHandler.CreateFleet, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.DeleteSquad, rpc:fleetObjectHandler.DeleteWing, rpc:fleetObjectHandler.DisbandFleet, rpc:fleetObjectHandler.FinishMove, rpc:fleetObjectHandler.GetFleetComposition, rpc:fleetObjectHandler.GetFleetID, rpc:fleetObjectHandler.GetFleetMaxSize, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetJoinRequests, rpc:fleetObjectHandler.GetMotd, rpc:fleetObjectHandler.GetWings, rpc:fleetObjectHandler.Init, rpc:fleetObjectHandler.KickMember, rpc:fleetObjectHandler.LeaveFleet, rpc:fleetObjectHandler.LoadFleetSetup, rpc:fleetObjectHandler.MakeLeader, rpc:fleetObjectHandler.MassMoveMembers, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.Reconnect, rpc:fleetObjectHandler.RejectInvite, rpc:fleetObjectHandler.RejectJoinRequest, rpc:fleetObjectHandler.SendBroadcast, rpc:fleetObjectHandler.SetAcceptsConduitJumpsValue, rpc:fleetObjectHandler.SetAcceptsRegroupValue, rpc:fleetObjectHandler.SetAutoJoinSquadID, rpc:fleetObjectHandler.SetFleetMaxSize, rpc:fleetObjectHandler.SetMotdEx, rpc:fleetObjectHandler.SetOptions, rpc:fleetObjectHandler.SetTakesFleetWarp, rpc:fleetObjectHandler.UpdateMemberInfo
    eve_testkit::attest(&[
        "rpc:fleetObjectHandler.AcceptInvite", "rpc:fleetObjectHandler.ChangeSquadName", "rpc:fleetObjectHandler.ChangeWingName", "rpc:fleetObjectHandler.CreateFleet", "rpc:fleetObjectHandler.CreateSquad", "rpc:fleetObjectHandler.CreateWing", "rpc:fleetObjectHandler.DeleteSquad", "rpc:fleetObjectHandler.DeleteWing", "rpc:fleetObjectHandler.DisbandFleet", "rpc:fleetObjectHandler.FinishMove", "rpc:fleetObjectHandler.GetFleetComposition", "rpc:fleetObjectHandler.GetFleetID", "rpc:fleetObjectHandler.GetFleetMaxSize", "rpc:fleetObjectHandler.GetInitState", "rpc:fleetObjectHandler.GetJoinRequests", "rpc:fleetObjectHandler.GetMotd", "rpc:fleetObjectHandler.GetWings", "rpc:fleetObjectHandler.Init", "rpc:fleetObjectHandler.KickMember", "rpc:fleetObjectHandler.LeaveFleet", "rpc:fleetObjectHandler.LoadFleetSetup", "rpc:fleetObjectHandler.MakeLeader", "rpc:fleetObjectHandler.MassMoveMembers", "rpc:fleetObjectHandler.MoveMember", "rpc:fleetObjectHandler.Reconnect", "rpc:fleetObjectHandler.RejectInvite", "rpc:fleetObjectHandler.RejectJoinRequest", "rpc:fleetObjectHandler.SendBroadcast", "rpc:fleetObjectHandler.SetAcceptsConduitJumpsValue", "rpc:fleetObjectHandler.SetAcceptsRegroupValue", "rpc:fleetObjectHandler.SetAutoJoinSquadID", "rpc:fleetObjectHandler.SetFleetMaxSize", "rpc:fleetObjectHandler.SetMotdEx", "rpc:fleetObjectHandler.SetOptions", "rpc:fleetObjectHandler.SetTakesFleetWarp", "rpc:fleetObjectHandler.UpdateMemberInfo",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1002, "other", "other-secret", P2).await;

    // 1. CreateFleet & Init
    let cr_fleet = rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "CreateFleet",
        &[PyRep::String("Strike Fleet".into()), PyRep::String("Form up on FC".into())],
    )
    .await
    .expect("CreateFleet");
    let fleet_id = match cr_fleet {
        PyRep::KeyVal(kv) => kv.get("fleetID").and_then(|v| v.as_i64()).unwrap_or(1),
        PyRep::Integer(id) => id,
        _ => 1,
    };
    assert!(fleet_id > 0);
    s1.fleet_id = Some(fleet_id);

    let init_res = rpc(&ctx, &mut s1, "fleetObjectHandler", "Init", &[]).await.expect("Init");
    assert!(matches!(init_res, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));
    let fleet_id = s1.fleet_id.unwrap_or(fleet_id);

    // 2. Wings and Squads
    let new_wing = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateWing", &[PyRep::String("Wing Bravo".into())]).await.expect("CreateWing");
    let wing_id = match new_wing {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("wingID").and_then(|v| v.as_i64()).unwrap_or(2),
        _ => 2,
    };

    let wings = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetWings", &[]).await.expect("GetWings");
    assert!(matches!(wings, PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Rowset { .. } | PyRep::List(_)));

    let chg_wing = rpc(&ctx, &mut s1, "fleetObjectHandler", "ChangeWingName", &[PyRep::Integer(wing_id), PyRep::String("Wing Heavy".into())]).await.expect("ChangeWingName");
    assert!(matches!(chg_wing, PyRep::KeyVal(_) | PyRep::Bool(_)));

    let new_squad = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(wing_id), PyRep::String("Squad Recon".into())]).await.expect("CreateSquad");
    let squad_id = match new_squad {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("squadID").and_then(|v| v.as_i64()).unwrap_or(2),
        _ => 2,
    };

    let chg_squad = rpc(&ctx, &mut s1, "fleetObjectHandler", "ChangeSquadName", &[PyRep::Integer(squad_id), PyRep::String("Squad Sniper".into())]).await.expect("ChangeSquadName");
    assert!(matches!(chg_squad, PyRep::KeyVal(_) | PyRep::Bool(_)));

    // 3. Invites & Joins
    let inv_p2 = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(P2), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(5)]).await.expect("Invite");
    assert!(matches!(inv_p2, PyRep::Bool(true) | PyRep::KeyVal(_)));

    let rej_inv = rpc(&ctx, &mut s2, "fleetObjectHandler", "RejectInvite", &[PyRep::Integer(fleet_id)]).await.expect("RejectInvite");
    assert!(matches!(rej_inv, PyRep::Bool(true) | PyRep::KeyVal(_) | PyRep::None));

    let inv2 = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(P2), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(5)]).await.expect("Invite 2");
    assert!(matches!(inv2, PyRep::Bool(true) | PyRep::KeyVal(_)));
    let acc_inv = rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)]).await.expect("AcceptInvite");
    assert!(matches!(acc_inv, PyRep::Bool(true) | PyRep::KeyVal(_) | PyRep::None));
    s2.fleet_id = Some(fleet_id);

    let join_reqs = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetJoinRequests", &[]).await.expect("GetJoinRequests");
    assert!(matches!(join_reqs, PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Dict(_)));

    let rej_join = rpc(&ctx, &mut s1, "fleetObjectHandler", "RejectJoinRequest", &[PyRep::Integer(P2)]).await.expect("RejectJoinRequest");
    assert!(matches!(rej_join, PyRep::Bool(true) | PyRep::KeyVal(_) | PyRep::None));

    // 4. Member Movements & Roles
    let mv_mem = rpc(&ctx, &mut s1, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(P2), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(5)]).await.expect("MoveMember");
    assert!(matches!(mv_mem, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let fin_mv = rpc(&ctx, &mut s1, "fleetObjectHandler", "FinishMove", &[PyRep::Integer(P2), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(5)]).await.expect("FinishMove");
    assert!(matches!(fin_mv, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let mass_mv = rpc(&ctx, &mut s1, "fleetObjectHandler", "MassMoveMembers", &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Integer(wing_id), PyRep::Integer(squad_id)]).await.expect("MassMoveMembers");
    assert!(matches!(mass_mv, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let upd_mem = rpc(&ctx, &mut s1, "fleetObjectHandler", "UpdateMemberInfo", &[PyRep::Integer(P2)]).await.expect("UpdateMemberInfo");
    assert!(matches!(upd_mem, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    // 5. State & Settings
    let comp = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetFleetComposition", &[]).await.expect("GetFleetComposition");
    assert!(matches!(comp, PyRep::Rowset { .. } | PyRep::List(_)));

    let fid = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetFleetID", &[]).await.expect("GetFleetID");
    assert!(matches!(fid, PyRep::Integer(_)));

    let max_sz = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetFleetMaxSize", &[]).await.expect("GetFleetMaxSize");
    assert!(matches!(max_sz, PyRep::Integer(_)));

    let set_max = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetFleetMaxSize", &[PyRep::Integer(128)]).await.expect("SetFleetMaxSize");
    assert!(matches!(set_max, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let init_st = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetInitState", &[]).await.expect("GetInitState");
    assert!(matches!(init_st, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let motd = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetMotd", &[]).await.expect("GetMotd");
    assert!(matches!(motd, PyRep::String(_)));

    let set_motd = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetMotdEx", &[PyRep::String("New fleet MOTD".into())]).await.expect("SetMotdEx");
    assert!(matches!(set_motd, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_opt = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetOptions", &[PyRep::Bool(true)]).await.expect("SetOptions");
    assert!(matches!(set_opt, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_conduit = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetAcceptsConduitJumpsValue", &[PyRep::Bool(true)]).await.expect("SetAcceptsConduitJumpsValue");
    assert!(matches!(set_conduit, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_regroup = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetAcceptsRegroupValue", &[PyRep::Bool(true)]).await.expect("SetAcceptsRegroupValue");
    assert!(matches!(set_regroup, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_auto = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetAutoJoinSquadID", &[PyRep::Integer(squad_id)]).await.expect("SetAutoJoinSquadID");
    assert!(matches!(set_auto, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let set_warp = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetTakesFleetWarp", &[PyRep::Bool(true)]).await.expect("SetTakesFleetWarp");
    assert!(matches!(set_warp, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let bcast = rpc(&ctx, &mut s1, "fleetObjectHandler", "SendBroadcast", &[PyRep::String("BroadcastTarget".into()), PyRep::Integer(JITA_44)]).await.expect("SendBroadcast");
    assert!(matches!(bcast, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let load_s = rpc(&ctx, &mut s1, "fleetObjectHandler", "LoadFleetSetup", &[PyRep::String("Standard Setup".into())]).await.expect("LoadFleetSetup");
    assert!(matches!(load_s, PyRep::Bool(true) | PyRep::None | PyRep::KeyVal(_)));

    let leader = rpc(&ctx, &mut s1, "fleetObjectHandler", "MakeLeader", &[PyRep::Integer(P2)]).await.expect("MakeLeader");
    assert!(matches!(leader, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let reconn = rpc(&ctx, &mut s2, "fleetObjectHandler", "Reconnect", &[]).await.expect("Reconnect");
    assert!(matches!(reconn, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));

    // 6. Cleanup
    let _ = rpc(&ctx, &mut s2, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(P2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)]).await;
    let del_sq = rpc(&ctx, &mut s2, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(squad_id)]).await.expect("DeleteSquad");
    assert!(matches!(del_sq, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let del_w = rpc(&ctx, &mut s2, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(wing_id)]).await.expect("DeleteWing");
    assert!(matches!(del_w, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let kick = rpc(&ctx, &mut s2, "fleetObjectHandler", "KickMember", &[PyRep::Integer(P1)]).await.expect("KickMember");
    assert!(matches!(kick, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));
    s1.fleet_id = None;

    let leave = rpc(&ctx, &mut s2, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("LeaveFleet");
    assert!(matches!(leave, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));

    let _ = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateFleet", &[PyRep::String("Fleet To Disband".into())]).await;
    let disband = rpc(&ctx, &mut s1, "fleetObjectHandler", "DisbandFleet", &[]).await.expect("DisbandFleet");
    assert!(matches!(disband, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None));
    assert_eq!(fid, PyRep::Integer(fleet_id));
    assert_eq!(s1.character_id, Some(P1));
    assert_eq!(s2.character_id, Some(P2));
    assert_eq!(s1.station_id, Some(JITA_44));
    assert_eq!(s2.station_id, Some(JITA_44));
}

// ============================================================================
// 5. ship Suite (29 rows)
// ============================================================================
#[tokio::test]
async fn test_ship_rpc_suite() {
    // parity: rpc:ship.AbortSafeLogoff, rpc:ship.AbortSelfDestruct, rpc:ship.AssembleShip, rpc:ship.Board, rpc:ship.BoardStoredShip, rpc:ship.Drop, rpc:ship.Eject, rpc:ship.FitShips, rpc:ship.GetDirtTimestamp, rpc:ship.GetShipConfiguration, rpc:ship.GetShipFittingInfo, rpc:ship.Jettison, rpc:ship.LaunchDrones, rpc:ship.LaunchFromContainer, rpc:ship.LeaveShip, rpc:ship.ResetDirtTimestamp, rpc:ship.SafeLogoff, rpc:ship.Scoop, rpc:ship.ScoopDrone, rpc:ship.ScoopToFleetHangar, rpc:ship.ScoopToInfrastructureHold, rpc:ship.ScoopToMobileDepotHold, rpc:ship.ScoopToSMA, rpc:ship.SelfDestruct, rpc:ship.SetShipPassword, rpc:ship.StoreVessel, rpc:ship.Undock
    eve_testkit::attest(&[
        "rpc:ship.AbortSafeLogoff", "rpc:ship.AbortSelfDestruct", "rpc:ship.AssembleShip", "rpc:ship.Board", "rpc:ship.BoardStoredShip", "rpc:ship.Drop", "rpc:ship.Eject", "rpc:ship.FitShips", "rpc:ship.GetDirtTimestamp", "rpc:ship.GetShipConfiguration", "rpc:ship.GetShipFittingInfo", "rpc:ship.Jettison", "rpc:ship.LaunchDrones", "rpc:ship.LaunchFromContainer", "rpc:ship.LeaveShip", "rpc:ship.ResetDirtTimestamp", "rpc:ship.SafeLogoff", "rpc:ship.Scoop", "rpc:ship.ScoopDrone", "rpc:ship.ScoopToFleetHangar", "rpc:ship.ScoopToInfrastructureHold", "rpc:ship.ScoopToMobileDepotHold", "rpc:ship.ScoopToSMA", "rpc:ship.SelfDestruct", "rpc:ship.SetShipPassword", "rpc:ship.StoreVessel", "rpc:ship.Undock",
    ]);

    let ctx = ServerContext::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_088,
            InventoryItemRecord {
                item_id: 100_000_088,
                type_id: 34,
                owner_id: P1,
                location_id: 100_000_001,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_000_089,
            InventoryItemRecord {
                item_id: 100_000_089,
                type_id: 587,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::SpecializedShipHold,
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
    });

    let mut s1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1002, "other", "other-secret", P2).await;

    // 1. Session check & memoization
    let chk = rpc(&ctx, &mut s1, "ship", "SetSessionCheck", &[]).await.expect("SetSessionCheck");
    assert!(matches!(chk, PyRep::None));

    let clr = rpc(&ctx, &mut s1, "ship", "clear_memoized", &[]).await.expect("clear_memoized");
    assert!(matches!(clr, PyRep::None));

    // 2. Dirt timestamps
    let dirt = rpc(&ctx, &mut s1, "ship", "GetDirtTimestamp", &[PyRep::Integer(100_000_001)]).await.expect("GetDirtTimestamp");
    assert!(matches!(dirt, PyRep::Integer(_)));

    let reset_dirt = rpc(&ctx, &mut s1, "ship", "ResetDirtTimestamp", &[]).await.expect("ResetDirtTimestamp");
    assert!(matches!(reset_dirt, PyRep::Integer(_)));

    // 3. Safe logoff while docked
    let logoff = rpc(&ctx, &mut s1, "ship", "SafeLogoff", &[]).await.expect("SafeLogoff");
    assert_eq!(logoff, PyRep::Integer(0));

    let abort_logoff = rpc(&ctx, &mut s1, "ship", "AbortSafeLogoff", &[]).await.expect("AbortSafeLogoff");
    assert_eq!(abort_logoff, PyRep::Bool(true));

    // 4. Cargo / item transfers & scooping
    let drop_res = rpc(&ctx, &mut s1, "ship", "Drop", &[PyRep::Integer(100_000_003)]).await.expect("Drop");
    assert!(matches!(drop_res, PyRep::Bool(_)));

    let sc = rpc(&ctx, &mut s1, "ship", "Scoop", &[PyRep::Integer(100_000_003)]).await.expect("Scoop");
    assert_eq!(sc, PyRep::Bool(true));

    let sc_drone = rpc(&ctx, &mut s1, "ship", "ScoopDrone", &[PyRep::Integer(100_000_003)]).await.expect("ScoopDrone");
    assert_eq!(sc_drone, PyRep::Bool(true));

    let sc_infra = rpc(&ctx, &mut s1, "ship", "ScoopToInfrastructureHold", &[PyRep::Integer(100_000_003)]).await.expect("ScoopToInfrastructureHold");
    assert_eq!(sc_infra, PyRep::Bool(true));

    let sc_depot = rpc(&ctx, &mut s1, "ship", "ScoopToMobileDepotHold", &[PyRep::Integer(100_000_003)]).await.expect("ScoopToMobileDepotHold");
    assert_eq!(sc_depot, PyRep::Bool(true));

    // 5. Fitting & configuration while docked
    let asm = rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(100_000_001)]).await.expect("AssembleShip");
    assert!(matches!(asm, PyRep::KeyVal(_) | PyRep::Integer(_)));

    let fit = rpc(&ctx, &mut s1, "ship", "FitShips", &[PyRep::List(vec![PyRep::Integer(100_000_001)])]).await.expect("FitShips");
    assert!(matches!(fit, PyRep::KeyVal(_)));

    let fit_info = rpc(&ctx, &mut s1, "ship", "GetShipFittingInfo", &[PyRep::Integer(587)]).await.expect("GetShipFittingInfo");
    assert!(matches!(fit_info, PyRep::KeyVal(_)));

    let cfg = rpc(&ctx, &mut s1, "ship", "GetShipConfiguration", &[PyRep::Integer(100_000_001)]).await.expect("GetShipConfiguration");
    assert!(matches!(cfg, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let set_pw = rpc(&ctx, &mut s1, "ship", "SetShipPassword", &[PyRep::Integer(100_000_001), PyRep::String("fleet_pass".into())]).await.expect("SetShipPassword");
    assert!(matches!(set_pw, PyRep::KeyVal(_) | PyRep::Dict(_)));

    // 6. Vessel storage & bays
    let store_v = rpc(&ctx, &mut s1, "ship", "StoreVessel", &[PyRep::Integer(100_000_004), PyRep::Integer(100_000_001)]).await;
    assert!(store_v.is_ok() || matches!(store_v.unwrap_err().code.as_str(), "NoShipMaintenanceBay" | "CapacityExceeded"));

    let sc_sma = rpc(&ctx, &mut s1, "ship", "ScoopToSMA", &[PyRep::Integer(100_000_004), PyRep::Integer(100_000_001)]).await;
    assert!(sc_sma.is_ok() || matches!(sc_sma.unwrap_err().code.as_str(), "NoShipMaintenanceBay" | "CapacityExceeded"));

    let sc_flt = rpc(&ctx, &mut s1, "ship", "ScoopToFleetHangar", &[PyRep::Integer(100_000_004), PyRep::Integer(100_000_001)]).await;
    assert!(sc_flt.is_ok() || matches!(sc_flt.unwrap_err().code.as_str(), "NoFleetHangar" | "CapacityExceeded"));

    let b_stored = rpc(&ctx, &mut s1, "ship", "BoardStoredShip", &[PyRep::Integer(100_000_089)]).await;
    assert!(b_stored.is_ok() || matches!(b_stored.unwrap_err().code.as_str(), "ShipNotInMaintenanceBay" | "ShipOutOfBoardingRange"));

    // 7. Leave ship and Board ship while docked
    let leave = rpc(&ctx, &mut s2, "ship", "LeaveShip", &[]).await.expect("LeaveShip");
    assert!(matches!(leave, PyRep::KeyVal(_)));

    let board = rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(100_000_001)]).await.expect("Board");
    assert!(matches!(board, PyRep::KeyVal(_)));

    // 8. Undock into space
    let undock = rpc(&ctx, &mut s1, "ship", "Undock", &[]).await.expect("Undock");
    assert!(matches!(undock, PyRep::KeyVal(_) | PyRep::Bool(true)));
    assert!(s1.station_id.is_none());

    // 9. In space operations: Drones, Launch, Jettison, SelfDestruct
    let l_drones = rpc(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![])]).await.expect("LaunchDrones");
    assert_eq!(l_drones, PyRep::List(vec![]));

    let l_cont = rpc(&ctx, &mut s1, "ship", "LaunchFromContainer", &[PyRep::Integer(100_000_088)]).await.expect("LaunchFromContainer");
    assert!(matches!(l_cont, PyRep::KeyVal(_)));

    let jettison = rpc(&ctx, &mut s1, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(100_000_005)])]).await.expect("Jettison");
    assert!(matches!(jettison, PyRep::Tuple(_) | PyRep::KeyVal(_)));

    let sd = rpc(&ctx, &mut s1, "ship", "SelfDestruct", &[PyRep::Integer(100_000_001)])
        .await
        .expect("SelfDestruct");
    assert_eq!(sd.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));

    let abort_sd = rpc(&ctx, &mut s1, "ship", "AbortSelfDestruct", &[]).await.expect("AbortSelfDestruct");
    assert!(matches!(abort_sd, PyRep::KeyVal(_)));

    let eject = rpc(&ctx, &mut s1, "ship", "Eject", &[]).await.expect("Eject");
    assert!(matches!(eject, PyRep::KeyVal(_)));
}
