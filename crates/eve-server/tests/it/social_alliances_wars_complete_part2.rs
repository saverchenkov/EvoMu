//! Phase 7 Corporation Deliveries, Goals, Bookmarks & Registry Integration Tests (`SO-3` Part 2).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None).await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

fn rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        PyRep::ObjectEx { list, .. } => {
            let r: Vec<Vec<PyRep>> = list
                .iter()
                .map(|item| {
                    if let PyRep::DBRow { columns, .. } = item {
                        columns.iter().map(|(_, v)| v.clone()).collect()
                    } else if let PyRep::List(cols) | PyRep::Tuple(cols) = item {
                        cols.clone()
                    } else {
                        vec![item.clone()]
                    }
                })
                .collect();
            (Vec::new(), r)
        }
        other => panic!("expected Rowset, got {other:?}"),
    }
}

fn col(cols: &[String], name: &str) -> usize {
    cols.iter().position(|c| c == name).unwrap_or_else(|| panic!("column {name} missing in {cols:?}"))
}

fn corp_wallet(ctx: &ServerContext, corp_id: i64) -> f64 {
    ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].wallet_divisions[0].balance.to_wire_f64())
}

fn set_corp_wallet(ctx: &ServerContext, corp_id: i64, isk: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.ensure_wallet_divisions();
        c.wallet_divisions[0].balance = eve_db::IskCents::from_isk(isk);
        c.master_wallet = c.wallet_divisions[0].balance;
    });
}

#[tokio::test]
async fn test_corporation_contacts_labels_ads_and_assets_rpc_tags() {
    // parity: rpc:corpRecProxy.CreateRecruitmentAd, rpc:corpRecProxy.DeleteRecruitmentAd, rpc:corpRecProxy.SearchCorpAds, rpc:corpRecProxy.UpdateRecruitmentAd, rpc:corpRegistry.AddCorporateContact, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.AssignLabels, rpc:corpRegistry.CanLeaveCurrentCorporation, rpc:corpRegistry.CreateLabel, rpc:corpRegistry.DeleteLabel, rpc:corpRegistry.EditLabel, rpc:corpRegistry.GetCorporateContacts, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.InsertInvitation, rpc:corpRegistry.KickOutMember
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

#[tokio::test]
async fn test_corporation_hq_deliveries_and_employment_rpc_tags() {
    // parity: rpc:corpRegistry.RegisterNewAggressionSettings, rpc:corpRegistry.RemoveCorporateContacts, rpc:corpRegistry.RemoveLabels, rpc:corpRegistry.UpdateApplicationOffer, rpc:corpRegistry.UpdateMember, rpc:corpRegistry.UpdateTitle, rpc:corpStationMgr.MoveCorpHQHere, rpc:corpmgr.GetAggressionSettings, rpc:corpmgr.GetAssetInventory, rpc:corpmgr.SearchAssets, rpc:corporationSvc.GetEmployementRecordAndCharacterTransfers, rpc:invbroker.DeliverToCorpHangar, rpc:invbroker.DeliverToCorpMember
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

#[tokio::test]
async fn test_corporation_deliveries_goals_freelance_fittings_assets_and_close() {
    // parity: feat:corporation.member-limit, feat:corporation.corp-hq, feat:corporation.corp-offices, feat:corporation.corp-hangars, feat:corporation.role-change-delay, feat:corporation.corp-deliveries, feat:corporation.corp-assets, feat:corporation.corp-fittings, feat:corporation.corp-aggression-settings, feat:corporation.corp-labels, feat:corporation.corp-contacts, feat:corporation.recruitment-ads, feat:corporation.corp-employment-records, feat:corporation.corp-close, rpc:charUnboundMgr.SelectCharacterID
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

async fn run_corporation_deliveries_and_governance_suite() -> (bool, bool, i64, i64, i64) {
    eve_testkit::attest(&[
        "feat:corporation.member-limit",
        "feat:corporation.corp-hq",
        "feat:corporation.corp-offices",
        "feat:corporation.corp-hangars",
        "feat:corporation.role-change-delay",
        "feat:corporation.corp-deliveries",
        "feat:corporation.corp-assets",
        "feat:corporation.corp-fittings",
        "feat:corporation.corp-aggression-settings",
        "feat:corporation.corp-labels",
        "feat:corporation.corp-contacts",
        "feat:corporation.recruitment-ads",
        "feat:corporation.corp-employment-records",
        "feat:corporation.corp-close",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(921);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut sess2 = SessionState::new(922);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        skills.insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        skills.insert(3732, eve_db::TrainedSkillRecord { type_id: 3732, level: 5, skill_points: 256_000.0 });
    });

    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Logistics & Projects Corp".into()), PyRep::String("LPRC".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    set_corp_wallet(&ctx, corp_id, 500_000_000);

    let attr_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_rep, "memberLimit"), PyRep::Integer(5 * 20 + 5 * 400));
    ctx.db.with_mut_snapshot(|snap| snap.skills.get_mut(&90_000_001).unwrap().clear());
    let attr_min = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_min, "memberLimit"), PyRep::Integer(1));
    let app_rep = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Apply".into())], &kw, None).await.unwrap();
    let app_id = field(&app_rep, "applicationID").as_i64().unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap_err().code, "CorporationFull");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "InsertInvitation", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap_err().code, "CorporationFull");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 });
    });
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = 0;

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let agg_init = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_init, "isFriendlyFireLegal"), PyRep::Bool(false));
    let agg_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_rep, "isFriendlyFireLegal"), PyRep::Bool(true));
    let agg_pend = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_pend, "isFriendlyFireLegal"), PyRep::Bool(false));
    assert_eq!(field(&agg_pend, "pendingFriendlyFireLegal"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    let agg_after = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_after, "isFriendlyFireLegal"), PyRep::Bool(true));

    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_761)], &kw, None).await.unwrap_err().code, "NoOfficeAtStation");
    let quote = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetPriceQuote", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_f64().unwrap();
    assert!(quote >= 10_000.0);
    let wallet_before_rent = corp_wallet(&ctx, corp_id);
    let rent_rep = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    let office_folder_id = field(&rent_rep, "officeFolderID").as_i64().unwrap();
    assert_eq!(corp_wallet(&ctx, corp_id), wallet_before_rent - quote);
    assert!(is_keyval(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffices", &[], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetCorporationsWithOffices", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetEmptyOfficeCount", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_i64().unwrap() >= 0);

    let hq_rep = ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(field(&hq_rep, "stationID"), PyRep::Integer(60_003_760));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_000_001, eve_db::InventoryItemRecord {
            item_id: 910_000_001, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 5_000, singleton: false, version: 1, ..Default::default()
        });
    });
    let del_hangar = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(1)], &kw, None).await.unwrap();
    assert_eq!(field(&del_hangar, "ownerID"), PyRep::Integer(corp_id));
    assert_eq!(field(&del_hangar, "flagID"), PyRep::Integer(116));

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    let div1_roles = (1u64 << 20) | (1u64 << 13);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(div1_roles as i64)], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.advance_secs(86_401);
    let div1_list = ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap();
    assert_eq!(rows(&div1_list).1.len(), 1);
    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(0)], &kw, None).await.unwrap();
    sess2.corp_role = 0;
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(2)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::CorpSAG2);
    let _ = office_folder_id;
    ctx.dispatch_rpc(&mut sess1, "officeManager", None, "UnrentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::Deliveries);

    let fit_kv = PyRep::keyval([("name", PyRep::String("Corp Doctrine Rifter".into())), ("shipTypeID", PyRep::Integer(587)), ("description", PyRep::String("Standard tackle".into())), ("fitData", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(438), PyRep::Integer(19), PyRep::Integer(1)])]))]);
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv.clone()], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("offices".into())], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let save_fit = ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv], &kw, None).await.unwrap();
    let fit_id = save_fit.as_i64().unwrap();
    let fits_rep = ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "GetFittings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    let (_, fits_rows) = rows(&fits_rep);
    let fit_row = fits_rows.iter().find(|r| r[0] == PyRep::Integer(fit_id)).unwrap();
    assert_eq!(&fit_row[1], &PyRep::String("Corp Doctrine Rifter".into()));
    assert_eq!(&fit_row[2], &PyRep::Integer(587));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_000_002, eve_db::InventoryItemRecord {
            item_id: 910_000_002, type_id: 34, owner_id: corp_id, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::CorpSAG1, quantity: 2_000, singleton: false, version: 1, ..Default::default()
        });
        snap.items.insert(910_000_003, eve_db::InventoryItemRecord {
            item_id: 910_000_003, type_id: 34, owner_id: corp_id, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Impounded, quantity: 500, singleton: false, version: 1, ..Default::default()
        });
    });

    let (_, asset_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("offices".into())], &kw, None).await.unwrap());
    assert_eq!(asset_rows.len(), 1);
    assert_eq!(asset_rows[0][0], PyRep::Integer(910_000_002));

    let (_, imp_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("impounded".into())], &kw, None).await.unwrap());
    assert_eq!(imp_rows.len(), 1);
    assert_eq!(imp_rows[0][0], PyRep::Integer(910_000_003));

    let (_, deliv_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("deliveries".into())], &kw, None).await.unwrap());
    assert_eq!(deliv_rows.len(), 1);
    assert_eq!(deliv_rows[0][0], PyRep::Integer(910_000_001));

    let (_, search_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "SearchAssets", &[PyRep::String("offices".into()), PyRep::None, PyRep::None, PyRep::Integer(34), PyRep::None], &kw, None).await.unwrap());
    assert_eq!(search_rows.len(), 1);
    assert_eq!(search_rows[0][0], PyRep::Integer(910_000_002));

    let del_member = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpMember", &[PyRep::Integer(910_000_001), PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&del_member, "ownerID"), PyRep::Integer(90_000_001));
    assert_eq!(field(&del_member, "flagID"), PyRep::Integer(eve_db::InventoryFlag::Deliveries as i64));

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(5.0)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "CreateLabel", &[PyRep::String("Blue".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(5.0)], &kw, None).await.unwrap();
    let lbl = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "CreateLabel", &[PyRep::String("Ally".into()), PyRep::Integer(2)], &kw, None).await.unwrap();
    let lbl_id = field(&lbl, "labelID").as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "EditLabel", &[PyRep::Integer(lbl_id), PyRep::String("Coalition".into()), PyRep::Integer(3)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AssignLabels", &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    let (_, c_rows) = rows(&ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "GetCorporateContacts", &[], &kw, None).await.unwrap());
    assert_eq!(c_rows[0][2], PyRep::Integer(lbl_id));
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "DeleteLabel", &[PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(7), PyRep::Integer(3), PyRep::String("Unauth Ad".into()), PyRep::String("Desc".into())], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let w_before_ad = corp_wallet(&ctx, corp_id);
    let ad_rep = ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(14), PyRep::Integer(4), PyRep::String("Nullsec PvP".into()), PyRep::String("Daily fleets".into())], &kw, None).await.unwrap();
    let ad_id = field(&ad_rep, "adID").as_i64().unwrap();
    assert!(corp_wallet(&ctx, corp_id) < w_before_ad);
    assert_eq!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(4)], &kw, None).await.unwrap()).1.len(), 1);
    assert!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(1)], &kw, None).await.unwrap()).1.is_empty());
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "UpdateRecruitmentAd", &[PyRep::Integer(ad_id), PyRep::Integer(12), PyRep::String("Updated PvP".into()), PyRep::String("Updated".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)], &kw, None).await.unwrap();

    let goal_rep = ctx.dispatch_rpc(&mut sess1, "corporationSvc", None, "CreateGoalSubmit", &[PyRep::String("Kill 10 Pirates".into()), PyRep::Integer(10), PyRep::Real(100_000.0), PyRep::String("DamageNPCShips".into()), PyRep::Bool(true)], &kw, None).await.unwrap();
    let goal_id = goal_rep.dict_get("projectID").and_then(|v| v.as_i64()).unwrap();
    let self_report = ctx.dispatch_rpc(&mut sess1, "corporationSvc", None, "GoalProgress", &[PyRep::Integer(goal_id), PyRep::Integer(10)], &kw, None).await.expect_err("client cannot self-report kills");
    assert_eq!(self_report.code, "ServerTrackedMatcher");
    let (fcols, frows) = rows(&ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "GetActiveFreelanceProjectId", &[], &kw, None).await.unwrap());
    assert!(frows.iter().any(|r| r[col(&fcols, "projectID")] == PyRep::Integer(goal_id)));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "CloseCorpProject", &[PyRep::Integer(goal_id)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Unauth".into()), PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let cw_before_close = corp_wallet(&ctx, corp_id);
    ctx.dispatch_rpc(&mut sess1, "corporationSvc", None, "CloseCorpProject", &[PyRep::Integer(goal_id)], &kw, None).await.unwrap();
    assert!(corp_wallet(&ctx, corp_id) > cw_before_close);

    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    let (_, emp_rows) = rows(&ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "GetEmployementRecordAndCharacterTransfers", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap());
    assert!(emp_rows.iter().any(|r| r[0] == PyRep::Integer(corp_id) || r.get(1) == Some(&PyRep::Integer(corp_id))));

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(999_123, eve_db::WarRecord { war_id: 999_123, aggressor_corp_id: corp_id, defender_corp_id: 1_000_002, mutual: false, started_unix: 0, active: true });
    });
    let can_leave_war = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "CanLeaveCurrentCorporation", &[], &kw, None).await.unwrap();
    assert_eq!(field(&can_leave_war, "canLeave"), PyRep::Bool(false));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap_err().code, "ActiveWarsPreventClosure");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&999_123).unwrap().active = false);
    let close_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&close_rep, "corporationClosed"), PyRep::Bool(true));
    let corp_closed = ctx.db.with_snapshot(|s| !s.corporations.contains_key(&corp_id));
    assert!(corp_closed);
    (corp_closed, true, 90_000_001, 90_000_002, 60_003_760)
}

#[tokio::test]
async fn test_access_group_bookmarks_full_lifecycle_and_management() {
    // parity: rpc:accessGroupBookmarkMgr.BookmarkStaticLocation, rpc:accessGroupBookmarkMgr.UpdateBookmark, rpc:accessGroupBookmarkMgr.DeleteBookmarks, rpc:accessGroupBookmarkMgr.GetFolderInfo, rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess, rpc:accessGroupBookmarkMgr.UpdateFolder, rpc:accessGroupBookmarkMgr.UpdateSubfolder, rpc:accessGroupBookmarkMgr.UpdateKnownFolderState
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess1 = SessionState::new(981);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();

    let corp_id = 98_000_555;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Bookmark Corp".into(),
            ticker: "BKMC".into(),
            ceo_id: 90_000_001,
            ..Default::default()
        };
        corp.member_roles.insert(90_000_001, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
        }
    });
    sess1.corporation_id = Some(corp_id);
    sess1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let PyRep::KeyVal(p_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Tacticals".into()), PyRep::Bool(false)], &kw, None).await.expect("AddFolder") else { panic!("expected KeyVal") };
    let p_fid = p_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::KeyVal(s_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Corp Ops".into()), PyRep::Bool(true)], &kw, None).await.expect("Add shared folder") else { panic!("expected KeyVal") };
    let s_fid = s_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();

    let PyRep::KeyVal(rn_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateFolder", &[PyRep::Integer(p_fid), PyRep::String("Renamed Tacticals".into())], &kw, None).await.expect("UpdateFolder") else { panic!("expected KeyVal") };
    assert_eq!(rn_kv.get("folderName"), Some(&PyRep::String("Renamed Tacticals".into())));

    let PyRep::KeyVal(sub_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CreateSubfolder", &[PyRep::Integer(p_fid), PyRep::String("Insta-Docks".into())], &kw, None).await.expect("CreateSubfolder") else { panic!("expected KeyVal") };
    let sub_fid = sub_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::KeyVal(upd_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateSubfolder", &[PyRep::Integer(p_fid), PyRep::String("Updated Docks".into())], &kw, None).await.expect("UpdateSubfolder") else { panic!("expected KeyVal") };
    assert!(upd_kv.get("folderID").is_some());

    let bm = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "BookmarkStaticLocation", &[PyRep::String("Station 1 Perimeter".into()), PyRep::Integer(60_003_760), PyRep::String("Perimeter undock bookmark".into()), PyRep::Integer(p_fid), PyRep::Integer(sub_fid), PyRep::Integer(0)], &kw, None).await.expect("BookmarkStaticLocation");
    let (bm_id, loc_id) = match &bm {
        PyRep::Tuple(parts) if parts.len() == 8 => (parts[0].as_i64().unwrap(), parts[6].as_i64().unwrap()),
        PyRep::KeyVal(bm_kv) => (bm_kv.get("bookmarkID").and_then(|v| v.as_i64()).unwrap(), bm_kv.get("locationID").and_then(|v| v.as_i64()).unwrap_or(0)),
        _ => panic!("expected 8-tuple or KeyVal"),
    };
    assert!(bm_id > 0);
    assert_eq!(loc_id, 30_000_142);

    let PyRep::KeyVal(ub_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateBookmark", &[PyRep::Integer(bm_id), PyRep::String("Station 1 Perimeter (Safe)".into()), PyRep::String("Updated perimeter memo".into()), PyRep::Integer(p_fid)], &kw, None).await.expect("UpdateBookmark") else { panic!("expected KeyVal") };
    assert_eq!(ub_kv.get("updated"), Some(&PyRep::Bool(true)));

    let PyRep::KeyVal(fi_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetFolderInfo", &[PyRep::Integer(p_fid)], &kw, None).await.expect("GetFolderInfo") else { panic!("expected KeyVal") };
    assert_eq!(fi_kv.get("folderID"), Some(&PyRep::Integer(p_fid)));

    let PyRep::Rowset { rows: af_rows, .. } = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "SearchFoldersWithAdminAccess", &[], &kw, None).await.expect("SearchFoldersWithAdminAccess") else { panic!("expected Rowset") };
    assert!(!af_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_fid))));
    assert!(af_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(s_fid))));

    let known_res = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateKnownFolderState", &[PyRep::Integer(s_fid), PyRep::Bool(true)], &kw, None).await.expect("UpdateKnownFolderState");
    let is_known = match &known_res {
        PyRep::Tuple(parts) => parts[0].dict_get("folderID") == Some(&PyRep::Integer(s_fid)),
        PyRep::KeyVal(kr_kv) => kr_kv.get("folderID") == Some(&PyRep::Integer(s_fid)),
        _ => false,
    };
    assert!(is_known);

    let del_bm = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])], &kw, None).await.expect("DeleteBookmarks");
    let is_deleted = match &del_bm {
        PyRep::List(del_ids) => del_ids.contains(&PyRep::Integer(bm_id)),
        PyRep::KeyVal(db_kv) => db_kv.get("deleted") == Some(&PyRep::Bool(true)),
        _ => false,
    };
    assert!(is_deleted);
}

#[tokio::test]
async fn test_corp_registry_members_and_tracking() {
    // parity: rpc:corpRegistry.GetMember, rpc:corpRegistry.GetMembersPaged, rpc:corpRegistry.GetMembersByIds, rpc:corpRegistry.GetMemberIDsByQuery, rpc:corpRegistry.GetMemberTrackingInfo, rpc:corpRegistry.GetMemberTrackingInfoSimple, rpc:corpRegistry.GetInfoWindowDataForChar, rpc:corpRegistry.UpdateMembers, rpc:corpRegistry.CanBeKickedOut, rpc:corpRegistry.GetPendingAutoKicks
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(1101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();

    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Omega Tactical Corps".into()), PyRep::String("OMTAC".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let cid = sess1.corporation_id.expect("corpID");

    assert!(!rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMember", vec![PyRep::Integer(cid)]).await.expect("GetMember")).1.is_empty());
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMembersPaged", vec![PyRep::Integer(cid)]).await.expect("GetMembersPaged")).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMembersByIds", vec![PyRep::Integer(cid), PyRep::List(vec![PyRep::Integer(90_000_001)])]).await.expect("GetMembersByIds")).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMemberIDsByQuery", vec![PyRep::Integer(cid)]).await.expect("GetMemberIDsByQuery")).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMemberTrackingInfo", vec![PyRep::Integer(cid)]).await.expect("GetMemberTrackingInfo")).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMemberTrackingInfoSimple", vec![PyRep::Integer(cid)]).await.expect("GetMemberTrackingInfoSimple")).1.len(), 1);

    let info = call(&ctx, &mut sess1, "corpRegistry", "GetInfoWindowDataForChar", vec![PyRep::Integer(cid)]).await.expect("GetInfoWindowDataForChar");
    assert_eq!(field(&info, "corporationID"), PyRep::Integer(cid));

    let upd_mem = call(&ctx, &mut sess1, "corpRegistry", "UpdateMembers", vec![PyRep::Integer(90_000_001), PyRep::Integer(0)]).await.expect("UpdateMembers");
    assert_eq!(field(&upd_mem, "characterID"), PyRep::Integer(90_000_001));
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "CanBeKickedOut", vec![PyRep::Integer(90_000_002)]).await.expect("CanBeKickedOut"), PyRep::Bool(true));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetPendingAutoKicks", vec![]).await.expect("GetPendingAutoKicks")));
}

#[tokio::test]
async fn test_corp_registry_operations_kills_and_naming() {
    // parity: rpc:corpRegistry.GetMyApplications, rpc:corpRegistry.GetOldApplications, rpc:corpRegistry.GetMyOldApplications, rpc:corpRegistry.DeleteAllianceApplication, rpc:corpRegistry.GetAggressionSettings, rpc:corpRegistry.UpdateCorporationAbilities, rpc:corpRegistry.GetSuggestedAllianceShortNames, rpc:corpRegistry.GetSuggestedTickerNames, rpc:corpRegistry.GetEveOwners, rpc:corpRegistry.GetRecentKills, rpc:corpRegistry.GetRecentLosses, rpc:corpRegistry.SetAccountKey, rpc:corpRegistry.KickOutMembers
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(1101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();
    let mut sess2 = SessionState::new(1102);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_002)]).await.unwrap();

    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Omega Tactical Corps".into()), PyRep::String("OMTAC".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let cid = sess1.corporation_id.expect("corpID");
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(77001, eve_db::KillmailRecord {
            kill_id: 77001, victim_character_id: 90_000_002, attacker_character_id: 90_000_001,
            solar_system_id: 30000142, ship_type_id: 587, total_value_isk: 500_000.0, killed_unix: 1700000000,
            victim_corp_id: 98_000_002, victim_alliance_id: None, attacker_corp_id: cid, attacker_alliance_id: None,
        });
    });

    assert!(is_rowset(&call(&ctx, &mut sess2, "corpRegistry", "GetMyApplications", vec![]).await.unwrap()));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetOldApplications", vec![]).await.unwrap()));
    assert!(is_rowset(&call(&ctx, &mut sess2, "corpRegistry", "GetMyOldApplications", vec![]).await.unwrap()));
    assert_eq!(field(&call(&ctx, &mut sess1, "corpRegistry", "DeleteAllianceApplication", vec![PyRep::Integer(99_000_001)]).await.unwrap(), "deleted"), PyRep::Bool(true));
    assert_eq!(field(&call(&ctx, &mut sess1, "corpRegistry", "GetAggressionSettings", vec![PyRep::Integer(cid)]).await.unwrap(), "corporationID"), PyRep::Integer(cid));
    assert!(field(&call(&ctx, &mut sess1, "corpRegistry", "UpdateCorporationAbilities", vec![]).await.unwrap(), "corporationID").as_i64().unwrap() > 0);
    assert!(matches!(call(&ctx, &mut sess1, "corpRegistry", "GetSuggestedAllianceShortNames", vec![PyRep::String("Northern Coalition Force".into())]).await.unwrap(), PyRep::List(l) if !l.is_empty()));
    assert!(matches!(call(&ctx, &mut sess1, "corpRegistry", "GetSuggestedTickerNames", vec![PyRep::String("Northern Mining Operations".into())]).await.unwrap(), PyRep::List(l) if !l.is_empty()));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetEveOwners", vec![]).await.unwrap()));
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetRecentKills", vec![]).await.unwrap()).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetRecentLosses", vec![]).await.unwrap()).1.len(), 0);
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "SetAccountKey", vec![PyRep::Integer(1001)]).await.unwrap(), PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.corporation_id = cid; }
        if let Some(corp) = snap.corporations.get_mut(&cid) { corp.members.insert(90_000_002); }
    });
    let kick_res = call(&ctx, &mut sess1, "corpRegistry", "KickOutMembers", vec![PyRep::Integer(90_000_002)]).await.expect("KickOutMembers");
    assert_eq!(field(&kick_res, "characterID"), PyRep::Integer(90_000_002));
}
