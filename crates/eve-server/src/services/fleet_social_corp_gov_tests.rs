//! Unit tests for `fleet_social_corp_gov` shares, dividends, titles, recruitment ads, and contacts.

#[cfg(test)]
#[path = "fleet_social_corp_gov_votes_tests.rs"]
mod fleet_social_corp_gov_votes_tests;

use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_and_clock,
};
use crate::services::fleet_social::handle;
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, ServerClock, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

pub(super) fn kv<'a>(rep: &'a PyRep, k: &str) -> &'a PyRep {
    rep.dict_get(k).unwrap_or_else(|| panic!("missing key {k}"))
}

pub(super) fn rs_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } => rows,
        _ => panic!("expected Rowset"),
    }
}

fn py_list(rep: &PyRep) -> &[PyRep] {
    match rep {
        PyRep::List(items) => items,
        _ => panic!("expected List"),
    }
}

pub(super) async fn call_gov(
    ctx: &crate::router::ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    if let Some(res) = handle(ctx, sess, svc, method, &args, &HashMap::new()).await {
        return res;
    }
    super::super::fleet_social_corps::handle_corps(ctx, sess, svc, method, &args, &HashMap::new())
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

#[tokio::test]
async fn corp_gov_shares_dividends_titles_and_ceo_resign() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let g = snap.corp_governance.entry(98_000_001).or_default();
        g.shares.insert(98_000_001, 1_000);
        g.company_shares = 1_000;
    });

    let err_move = call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "MoveCompanyShares",
        vec![PyRep::Integer(98_000_001), PyRep::Integer(90_000_002), PyRep::Integer(100)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_move.code, "CorpRoleDenied");

    let mv = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MoveCompanyShares",
        vec![PyRep::Integer(98_000_001), PyRep::Integer(90_000_002), PyRep::Integer(400)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&mv, "sharesTransferred").as_i64(), Some(400));

    let pv = call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "MovePrivateShares",
        vec![PyRep::Integer(98_000_001), PyRep::Integer(90_000_001), PyRep::Integer(150)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&pv, "sharesTransferred").as_i64(), Some(150));

    let by_sh = call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "GetSharesByShareholder",
        vec![PyRep::Bool(false)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&by_sh)[0][2].as_i64(), Some(250));

    let holders = call_gov(&ctx, &mut ceo, "corpRegistry", "GetShareholders", vec![])
        .await
        .unwrap();
    let PyRep::IndexRowset {
        key_column,
        columns,
        items,
    } = holders
    else {
        panic!("expected IndexRowset for GetShareholders");
    };
    assert_eq!(key_column, "shareholderID");
    assert_eq!(
        columns,
        vec![
            "shareholderID",
            "corporationID",
            "shareholderCorporationID",
            "shares"
        ]
    );
    assert_eq!(items.len(), 3);
    let ceo_entry = items
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(90_000_001))
        .expect("CEO shareholder entry");
    assert_eq!(ceo_entry.1[2], PyRep::Integer(98_000_001));
    let corp_entry = items
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(98_000_001))
        .expect("Corp shareholder entry");
    assert_eq!(corp_entry.1[2], PyRep::None);

    let above_avg = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "GetMemberIDsWithMoreThanAvgShares",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(py_list(&above_avg), &[PyRep::Integer(90_000_002)]);

    let err_div = call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "PayoutDividend",
        vec![PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_div.code, "CorpRoleDenied");

    let ceo_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let mem_before = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
    let div_ok = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "PayoutDividend",
        vec![PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&div_ok, "recipientCount").as_i64(), Some(2));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0 - ceo_before), 150_000 * 100);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0 - mem_before), 250_000 * 100);

    let div_mem = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "PayoutDividend",
        vec![PyRep::Bool(false), PyRep::Real(500_000.0), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&div_mem, "recipientCount").as_i64(), Some(2));

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.entry(98_000_001).or_default().member_titles.insert(90_000_002, 1);
    });
    let upd_title = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitle",
        vec![
            PyRep::Integer(1),
            PyRep::String("Fleet Commander".into()),
            PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    assert_eq!(kv(&upd_title, "titleName").as_str(), Some("Fleet Commander"));
    let titles = call_gov(&ctx, &mut ceo, "corpRegistry", "GetTitles", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&titles).len(), 1);
    let del_title = call_gov(&ctx, &mut ceo, "corpRegistry", "DeleteTitle", vec![PyRep::Integer(1)])
        .await
        .unwrap();
    assert_eq!(kv(&del_title, "deleted").as_bool(), Some(true));

    let err_resign = call_gov(&ctx, &mut ceo, "corpRegistry", "ResignFromCEO", vec![])
        .await
        .unwrap_err();
    assert_eq!(err_resign.code, "InvalidNewCEO");

    let ok_resign = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "ResignFromCEO",
        vec![PyRep::Integer(90_000_002)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&ok_resign, "newCeoID").as_i64(), Some(90_000_002));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&98_000_001].ceo_id), 90_000_002);
    let _ = clock;
}

#[tokio::test]
async fn corp_gov_recruitment_contacts_labels_and_add_corp() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);

    let created_ad = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::String("Industrial & PvP".into()),
            PyRep::String("Active USTZ/EUTZ pilots".into()),
            PyRep::Integer(7),
            PyRep::Integer(14),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap();
    let ad_id = kv(&created_ad, "adID").as_i64().unwrap();

    let upd_ad = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(3),
            PyRep::String("Updated PvP Corp".into()),
            PyRep::String("Small gang fleets daily".into()),
            PyRep::Integer(2),
        ],
    )
    .await
    .unwrap();
    assert_eq!(kv(&upd_ad, "title").as_str(), Some("Updated PvP Corp"));

    let recruiters = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "GetRecruiters",
        vec![PyRep::Integer(98_000_001), PyRep::Integer(ad_id)],
    )
    .await
    .unwrap();
    assert_eq!(py_list(&recruiters), &[PyRep::Integer(90_000_001)]);

    let by_corp = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "GetRecruitmentAdsForCorporation",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&by_corp).len(), 1);

    let searched = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "SearchCorpAds",
        vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(1), PyRep::Integer(100)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&searched).len(), 1);

    let err_del = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "DeleteRecruitmentAd",
        vec![PyRep::Integer(ad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_del.code, "CorpRoleDenied");

    let ok_del = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "DeleteRecruitmentAd",
        vec![PyRep::Integer(ad_id)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&ok_del, "deleted").as_bool(), Some(true));

    let err_lbl = call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "CreateLabel",
        vec![PyRep::String("Blue Donut".into()), PyRep::Integer(0x00FF00)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_lbl.code, "CorpRoleDenied");

    let lbl = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "CreateLabel",
        vec![PyRep::String("Coalition Allies".into()), PyRep::Integer(0x00AAFF)],
    )
    .await
    .unwrap();
    let lid = kv(&lbl, "labelID").as_i64().unwrap();
    let edit_lbl = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "EditLabel",
        vec![PyRep::Integer(lid), PyRep::String("Core Allies".into()), PyRep::Integer(0x112233)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&edit_lbl, "name").as_str(), Some("Core Allies"));
    let labels = call_gov(&ctx, &mut member, "corpRegistry", "GetLabels", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&labels).len(), 1);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.insert(
            98_000_001,
            vec![eve_db::SocialContactRecord {
                contact_id: 98_000_002,
                contact_type: "corporation".into(),
                standing: 10.0,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![lid],
            }],
        );
    });
    let contacts = call_gov(&ctx, &mut member, "corpRegistry", "GetCorporateContacts", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&contacts)[0][0].as_i64(), Some(98_000_002));
    let rem_c = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "RemoveCorporateContacts",
        vec![PyRep::Integer(98_000_002)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&rem_c, "removed").as_bool(), Some(true));

    clock.advance_secs(5);
    let mut founder = member.clone();
    founder.character_id = Some(90_000_099);
    founder.corporation_id = Some(1_000_125);
    founder.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, 90_000_099, "Corp Founder", 1_000_125, 30_000_142, 60_003_760);
        ch.station_id = Some(60_003_760);
        ch.balance = IskCents::from_isk(10_000_000);
        snap.characters.insert(90_000_099, ch);
        snap.skills.entry(90_000_099).or_default().insert(
            3363,
            TrainedSkillRecord { type_id: 3363, level: 2, skill_points: 1000.0 },
        );
        snap.items.insert(
            55_000_123,
            InventoryItemRecord {
                item_id: 55_000_123,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let new_corp = call_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        vec![
            PyRep::String("Gamma Syndicate".into()),
            PyRep::String("GAMA".into()),
            PyRep::String("PvP Corp".into()),
            PyRep::String("https://gamma.eve".into()),
            PyRep::Real(5.0),
        ],
    )
    .await
    .unwrap();
    let new_cid = kv(&new_corp, "corporationID").as_i64().unwrap();
    let divs = call_gov(
        &ctx,
        &mut founder,
        "account",
        "GetWalletDivisionsInfo",
        vec![PyRep::Integer(new_cid)],
    )
    .await
    .unwrap();
    let PyRep::Rowset { columns, rows } = &divs else {
        panic!("expected Rowset from GetWalletDivisionsInfo");
    };
    assert!(columns.contains(&"key".to_string()));
    assert_eq!(rows.len(), 7);
    let key_idx = columns.iter().position(|c| c == "key").unwrap();
    let keys: Vec<i64> = rows.iter().filter_map(|r| r[key_idx].as_i64()).collect();
    assert_eq!(keys, vec![1000, 1001, 1002, 1003, 1004, 1005, 1006]);
}
