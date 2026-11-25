//! Unit tests for `fleet_social_corp_gov` shares, dividends, titles, recruitment ads, and contacts.

#[cfg(test)]
#[path = "fleet_social_corp_gov_votes_tests.rs"]
mod fleet_social_corp_gov_votes_tests;

use crate::services::fleet_social::fleet_social_corp_gov::{
    parse_title_dict_entry, parse_title_row_cells, parse_title_update_rows,
};
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
    let PyRep::Dict(title_entries) = &titles else {
        panic!("expected Dict for GetTitles, got {titles:?}");
    };
    assert_eq!(title_entries.len(), 16);
    assert_eq!(title_entries[0].0.as_i64(), Some(1));
    assert_eq!(title_entries[0].1.dict_get("titleName").and_then(|v| v.as_str()), Some("Fleet Commander"));
    assert_eq!(title_entries[15].0.as_i64(), Some(32_768));

    // UpdateTitles with a V24 Rowset containing power-of-two titleIDs 32 and 32768
    let rowset_update = PyRep::rowset(
        vec![
            "titleID",
            "titleName",
            "roles",
            "grantableRoles",
            "rolesAtHQ",
            "grantableRolesAtHQ",
            "rolesAtBase",
            "grantableRolesAtBase",
            "rolesAtOther",
            "grantableRolesAtOther",
        ],
        vec![
            vec![
                PyRep::Integer(32),
                PyRep::String("Logistics Chief".into()),
                PyRep::Integer(corp_roles::ROLE_FACTORY_MANAGER as i64),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
            vec![
                PyRep::Integer(32_768),
                PyRep::String("Senior Admiral".into()),
                PyRep::Integer(corp_roles::ROLE_STATION_MANAGER as i64),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        ],
    );
    call_gov(&ctx, &mut ceo, "corpRegistry", "UpdateTitles", vec![rowset_update])
        .await
        .unwrap();
    let titles_after_batch = call_gov(&ctx, &mut ceo, "corpRegistry", "GetTitles", vec![])
        .await
        .unwrap();
    let PyRep::Dict(batch_entries) = &titles_after_batch else {
        panic!("expected Dict for GetTitles, got {titles_after_batch:?}");
    };
    assert_eq!(batch_entries.len(), 16);
    let row_32 = batch_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(32))
        .map(|(_, v)| v)
        .expect("titleID 32");
    assert_eq!(row_32.dict_get("titleName").and_then(|v| v.as_str()), Some("Logistics Chief"));
    assert_eq!(row_32.dict_get("roles").and_then(|v| v.as_i64()), Some(corp_roles::ROLE_FACTORY_MANAGER as i64));
    let row_32768 = batch_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(32_768))
        .map(|(_, v)| v)
        .expect("titleID 32768");
    assert_eq!(row_32768.dict_get("titleName").and_then(|v| v.as_str()), Some("Senior Admiral"));

    // Verify member with titleMask = 32 | 32768 inherits both roles via effective_corp_roles_at_db
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .member_titles
            .insert(90_000_002, 32 | 32_768);
    });
    let (_, eff_roles) = ctx
        .db
        .with_snapshot(|s| s.effective_corp_roles_at_db(98_000_001, 90_000_002, None, 1_700_000_000));
    assert_ne!(eff_roles & corp_roles::ROLE_FACTORY_MANAGER, 0);
    assert_ne!(eff_roles & corp_roles::ROLE_STATION_MANAGER, 0);

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
    let PyRep::IndexRowset {
        key_column: contact_key_col,
        columns: contact_cols,
        items: contact_items,
    } = contacts
    else {
        panic!("expected IndexRowset for GetCorporateContacts");
    };
    assert_eq!(contact_key_col, "contactID");
    assert_eq!(contact_cols, vec!["contactID", "relationshipID", "labelMask"]);
    assert_eq!(contact_items.len(), 1);
    assert_eq!(contact_items[0].0, PyRep::Integer(98_000_002));
    assert_eq!(contact_items[0].1[0].as_i64(), Some(98_000_002));
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
    let empty_contacts = call_gov(&ctx, &mut member, "corpRegistry", "GetCorporateContacts", vec![])
        .await
        .unwrap();
    assert!(matches!(
        empty_contacts,
        PyRep::IndexRowset { ref key_column, ref items, .. }
            if key_column == "contactID" && items.is_empty()
    ));

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

    // Verify AddCorporation seeded the 16 power-of-two titles
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_titles[&new_cid].len(), 16);
        assert_eq!(s.corp_titles[&new_cid][0].title_id, 1);
        assert_eq!(s.corp_titles[&new_cid][15].title_id, 32_768);
    });
}

#[tokio::test]
async fn create_recruitment_ad_rejects_insufficient_isk_and_emits_account_change() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, _member) = seed_player_corps(&ctx);
    let char_id = ceo.character_id.unwrap();
    let corp_id = ceo.corporation_id.unwrap();
    let uid = ceo.user_id.unwrap_or(char_id as u64);
    ceo.user_id = Some(uid);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.set_division_balance(1, IskCents::from_isk(0));
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.balance = IskCents::from_isk(100_000);
        }
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            let uname = format!("u{uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(uid, ceo.session_id, Some(char_id), true, 1_700_000_000)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Corp div 1 has 0 ISK even though founder has 1,000,000 ISK -> strict NotEnoughISK, personal wallet untouched, no ads/notifications.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let err = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(14),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Should fail".into()),
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::String("Underfunded V24 Ad".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughISK");
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, IskCents::from_isk(1_000_000));
        assert!(s.fleet_and_social_ops.corp_recruitment_ads.iter().all(|a| a.corp_id != corp_id));
        assert!(s.corp_governance.get(&corp_id).map_or(true, |g| g.recruitment_ads.is_empty()));
    });

    // 2. Reject invalid days (< 1 or > 28) and negative bitmasks via checked casts before any debit.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            corp.set_division_balance(2, IskCents::from_isk(1_000_000));
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    for bad_args in [
        vec![
            PyRep::Integer(-1), PyRep::Integer(7), PyRep::Integer(1), PyRep::String("Bad".into()),
            PyRep::List(vec![]), PyRep::String("Bad Days -1".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
        ],
        vec![
            PyRep::Integer(29), PyRep::Integer(7), PyRep::Integer(1), PyRep::String("Bad".into()),
            PyRep::List(vec![]), PyRep::String("Bad Days 29".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
        ],
        vec![
            PyRep::Integer(14), PyRep::Integer(-2), PyRep::Integer(1), PyRep::String("Bad".into()),
            PyRep::List(vec![]), PyRep::String("Bad TypeMask".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
        ],
        vec![
            PyRep::Integer(14), PyRep::Integer(7), PyRep::Integer(-5), PyRep::String("Bad".into()),
            PyRep::List(vec![]), PyRep::String("Bad LangMask".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
        ],
    ] {
        assert_eq!(
            call_gov(&ctx, &mut ceo, "corpRecProxy", "CreateRecruitmentAd", bad_args)
                .await
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
    }
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(1_000_000));
    });

    // 3. Canonical V24 9-arg CreateRecruitmentAd debits active corp_account_key division (div 2 / 1001),
    // parses title (args[5]), langMask (args[2]), and recruiters (args[4]) atomically, and UpdateRecruitmentAd (8-arg V24) updates them.
    ceo.corp_account_key = 1001;
    let ad_v24 = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(21),
            PyRep::Integer(12),
            PyRep::Integer(4),
            PyRep::String("USTZ Nullsec Recruitment".into()),
            PyRep::List(vec![PyRep::Integer(char_id), PyRep::Integer(90_000_002)]),
            PyRep::String("V24 Title".into()),
            PyRep::Integer(0),
            PyRep::Integer(5_000_000),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    let ad_v24_id = kv(&ad_v24, "adID").as_i64().unwrap();
    assert_eq!(kv(&ad_v24, "title").as_str(), Some("V24 Title"));
    let n_corp = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        n_corp
            .iter()
            .filter(|n| n.event == "OnAccountChange")
            .map(|n| n.args.clone())
            .collect::<Vec<_>>(),
        vec![vec![
            PyRep::String("cash2".into()),
            PyRep::Integer(corp_id),
            PyRep::Real(750_000.0),
        ]]
    );
    assert_eq!(
        n_corp
            .iter()
            .filter(|n| n.event == "OnCorporationRecruitmentAdChanged")
            .count(),
        1
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(1_000_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(750_000));
        assert_eq!(s.characters[&char_id].balance, IskCents::from_isk(1_000_000));
        let stored = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_v24_id)
            .expect("ad persisted");
        assert_eq!(stored.title, "V24 Title");
        assert_eq!(stored.description, "USTZ Nullsec Recruitment");
        assert_eq!(stored.days, 21);
        assert_eq!(stored.type_mask, 12);
        assert_eq!(stored.lang_mask, 4);
        assert!(s.corp_wallet_journals.get(&(corp_id, 2)).is_some_and(|rows| {
            rows.iter().any(|r| {
                r.ref_type == "corp_recruitment_ad_fee"
                    && (r.amount - (-250_000.0)).abs() < 1e-6
                    && (r.balance - 750_000.0).abs() < 1e-6
            })
        }));
    });
    // Canonical 1-arg V24 GetRecruiters(adID) returns [char_id, 90_000_002], and 2-arg remains compatible.
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(ad_v24_id)],
        )
        .await
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(char_id), PyRep::Integer(90_000_002)])
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(corp_id), PyRep::Integer(ad_v24_id)],
        )
        .await
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(char_id), PyRep::Integer(90_000_002)])
    );

    // 4. Canonical V24 8-arg UpdateRecruitmentAd(adID, typeMask, langMask, description, recruiters, title, addedDays, hourMask1):
    // extending with addedDays > 0 debits active corp wallet division (750k -> 500k), writes journal, emits OnAccountChange,
    // and caps expiry_unix at now_unix + 28 * 86_400.
    let upd_v24 = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_v24_id),
            PyRep::Integer(8),
            PyRep::Integer(16),
            PyRep::String("Updated V24 Desc".into()),
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::String("Updated V24 Title".into()),
            PyRep::Integer(14),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    assert_eq!(kv(&upd_v24, "title").as_str(), Some("Updated V24 Title"));
    let n_ext = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        n_ext
            .iter()
            .filter(|n| n.event == "OnAccountChange")
            .map(|n| n.args.clone())
            .collect::<Vec<_>>(),
        vec![vec![
            PyRep::String("cash2".into()),
            PyRep::Integer(corp_id),
            PyRep::Real(500_000.0),
        ]]
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(500_000));
        let stored = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_v24_id)
            .unwrap();
        assert_eq!(stored.title, "Updated V24 Title");
        assert_eq!(stored.lang_mask, 16);
        assert_eq!(stored.days, 28);
        assert_eq!(stored.expiry_unix, 1_700_000_000 + 28 * 86_400);
        assert_eq!(stored.recruiter_id, 90_000_002);
        let ext_journals = s.corp_wallet_journals.get(&(corp_id, 2)).unwrap();
        assert_eq!(
            ext_journals
                .iter()
                .filter(|r| r.ref_type == "corp_recruitment_ad_fee")
                .count(),
            2
        );
    });

    // 5. Reject addedDays extension when active division lacks ISK, without mutating ad.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .set_division_balance(2, IskCents::from_isk(100_000));
    });
    let err_ext_isk = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_v24_id),
            PyRep::Integer(8),
            PyRep::Integer(16),
            PyRep::String("Should not apply".into()),
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::String("Unfunded Ext".into()),
            PyRep::Integer(7),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_ext_isk.code, "NotEnoughISK");
    ctx.db.with_snapshot(|s| {
        let stored = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_v24_id)
            .unwrap();
        assert_eq!(stored.title, "Updated V24 Title");
    });

    // 6. Reject non-member recruiter IDs and oversized title/description bounds, allow clearing langMask to 0,
    // and preserve explicit empty recruiters = [].
    let err_non_member = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_v24_id),
            PyRep::Integer(8),
            PyRep::Integer(0),
            PyRep::String("Bad recruiter".into()),
            PyRep::List(vec![PyRep::Integer(90_099_999)]),
            PyRep::String("Reject NonMember".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_member.code, "InvalidArgument");
    let err_non_member_create = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::String("Desc".into()),
            PyRep::List(vec![PyRep::Integer(90_099_999)]),
            PyRep::String("Bad Recruiter Create".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_member_create.code, "InvalidArgument");

    let err_long_title = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_v24_id),
            PyRep::Integer(8),
            PyRep::Integer(0),
            PyRep::String("Desc".into()),
            PyRep::List(vec![]),
            PyRep::String("T".repeat(129)),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_long_title.code, "InvalidArgument");

    call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_v24_id),
            PyRep::Integer(8),
            PyRep::Integer(0),
            PyRep::String("Cleared Lang & Recruiters".into()),
            PyRep::List(vec![]),
            PyRep::String("Empty Recruiters".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|s| {
        let stored = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_v24_id)
            .unwrap();
        assert_eq!(stored.lang_mask, 0);
        assert_eq!(stored.recruiter_id, 0);
    });
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(ad_v24_id)],
        )
        .await
        .unwrap(),
        PyRep::List(vec![])
    );
    ceo.corp_account_key = 1000;
}

#[tokio::test]
async fn recruitment_ad_paid_create_and_extend_require_active_division_wallet_take_role_and_respect_stasis() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);
    let corp_id = ceo.corporation_id.unwrap();
    let mem_id = member.character_id.unwrap();
    let mem_uid = mem_id as u64;
    member.user_id = Some(mem_uid);

    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.set_division_balance(1, IskCents::from_isk(1_000_000));
        corp.set_division_balance(2, IskCents::from_isk(1_000_000));
        // Member has Personnel Manager + Junior Accountant (can switch corpAccountKey to div 2), no wallet take.
        corp.member_roles.insert(
            mem_id,
            corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_JUNIOR_ACCOUNTANT,
        );
        // Delayed grant of ROLE_ACCOUNT_CAN_TAKE[1] (div 2) still under 24h stasis.
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(mem_id, (corp_roles::ROLE_ACCOUNT_CAN_TAKE[1], 1_700_000_000 + 86_400));
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.user_id = mem_uid;
        }
        if !snap.accounts.values().any(|a| a.user_id == mem_uid) {
            let uname = format!("u{mem_uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: mem_uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(mem_uid, member.session_id, Some(mem_id), true, 1_700_000_000)
        .unwrap();

    // Junior Accountant switches active division to 1001 (div 2); stale session bits must NOT authorize spending.
    crate::services::corp_registry_ops::handle(
        &ctx,
        &mut member,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(1001)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(member.corp_account_key, 1001);
    member.corp_role = corp_roles::ROLE_PERSONNEL_MANAGER
        | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
        | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1];
    let _ = ctx.drain_notifications_for_char(mem_id);

    // 1. CreateRecruitmentAd rejected with CorpRoleDenied before 24h wallet-take stasis expires.
    let err_create = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::String("Unauthorized spend".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Denied Ad".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_create.code, "CorpRoleDenied");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(1_000_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(1_000_000));
        assert!(s.fleet_and_social_ops.corp_recruitment_ads.iter().all(|a| a.corp_id != corp_id));
    });

    // 2. CEO creates an initial ad from div 1 (1_000_000 -> 750_000 ISK).
    let created = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::String("Initial Desc".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Initial Ad".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    let ad_id = kv(&created, "adID").as_i64().unwrap();
    let orig_expiry = ctx.db.with_snapshot(|s| {
        s.fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_id)
            .unwrap()
            .expiry_unix
    });

    // 3. Free edit (addedDays = 0) succeeds for Personnel Manager without wallet take and spends 0 ISK.
    call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::String("Edited Free".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Free Edit Title".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|s| {
        let ad = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_id)
            .unwrap();
        assert_eq!(ad.title, "Free Edit Title");
        assert_eq!(ad.expiry_unix, orig_expiry);
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(750_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(1_000_000));
    });

    // 4. Paid extend (addedDays = 7) rejected under stasis, and still rejected when only div-1 take is granted.
    let err_extend_stasis = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::String("Blocked Extend".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Blocked Title".into()),
            PyRep::Integer(7),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_extend_stasis.code, "CorpRoleDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            mem_id,
            corp_roles::ROLE_PERSONNEL_MANAGER
                | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
        );
    });
    let err_create_wrong_div = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::String("Wrong Div".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Wrong Div Ad".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_create_wrong_div.code, "CorpRoleDenied");
    let err_extend_wrong_div = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::String("Wrong Div Extend".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Wrong Div Title".into()),
            PyRep::Integer(7),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_extend_wrong_div.code, "CorpRoleDenied");

    ctx.db.with_snapshot(|s| {
        let ad = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_id)
            .unwrap();
        assert_eq!(ad.title, "Free Edit Title");
        assert_eq!(ad.expiry_unix, orig_expiry);
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(750_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(1_000_000));
    });

    // 5. Advance past 24h stasis so ROLE_ACCOUNT_CAN_TAKE[1] matures -> Create & paid Extend succeed from div 2.
    clock.advance_secs(86_405);
    call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(7),
            PyRep::Integer(2),
            PyRep::Integer(1),
            PyRep::String("Matured Create".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Matured Ad".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::String("Matured Extend".into()),
            PyRep::List(vec![PyRep::Integer(mem_id)]),
            PyRep::String("Extended Title".into()),
            PyRep::Integer(7),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|s| {
        let ad = s
            .fleet_and_social_ops
            .corp_recruitment_ads
            .iter()
            .find(|a| a.ad_id == ad_id)
            .unwrap();
        assert_eq!(ad.title, "Extended Title");
        assert!(ad.expiry_unix > orig_expiry);
        assert_eq!(s.corporations[&corp_id].division_balance(1), IskCents::from_isk(750_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(500_000));
    });
}

#[tokio::test]
async fn add_corporation_v24_15_args_persists_typeface_friendly_fire_and_lp_tax_rate() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (_ceo, mut founder) = seed_player_corps(&ctx);
    let char_id = founder.character_id.unwrap();
    let station_id = 60_003_760_i64;
    let uid = char_id as u64;
    founder.user_id = Some(uid);
    founder.station_id = Some(station_id);
    founder.ship_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(5_000_000);
        }
        snap.skills
            .entry(char_id)
            .or_default()
            .insert(
                3363,
                TrainedSkillRecord {
                    type_id: 3363,
                    skill_points: 100_000.0,
                    level: 2,
                },
            );
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            let uname = format!("u{uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(uid, founder.session_id, Some(char_id), true, 1_700_000_000)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let created = call_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        vec![
            PyRep::String("LP Frontier".into()),
            PyRep::String("LPFR".into()),
            PyRep::String("Desc".into()),
            PyRep::String("https://lp.example".into()),
            PyRep::Real(0.05),
            PyRep::Integer(415),
            PyRep::Integer(416),
            PyRep::Integer(417),
            PyRep::Integer(671),
            PyRep::Integer(672),
            PyRep::Integer(673),
            PyRep::String("Futura".into()),
            PyRep::Bool(false),
            PyRep::Bool(true),
            PyRep::Real(0.22),
        ],
    )
    .await
    .unwrap();
    let new_corp_id = kv(&created, "corporationID").as_i64().unwrap();
    let notifs = ctx.drain_notifications_for_char(char_id);
    let corp_changed = notifs
        .iter()
        .find(|n| n.event == "OnCorporationChanged")
        .expect("OnCorporationChanged emitted");
    let chg_dict = &corp_changed.args[1];
    assert_eq!(
        chg_dict.dict_get("typeface"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::String("Futura".into())]))
    );
    assert_eq!(
        chg_dict.dict_get("friendlyFire"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::Bool(true)]))
    );
    assert_eq!(
        chg_dict.dict_get("loyaltyPointTaxRate"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::Real(0.22)]))
    );
    assert_eq!(
        chg_dict.dict_get("applicationsEnabled"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::Bool(false)]))
    );

    let fetched = call_gov(&ctx, &mut founder, "corpRegistry", "GetCorporation", vec![])
        .await
        .unwrap();
    assert_eq!(kv(&fetched, "corporationID").as_i64(), Some(new_corp_id));
    assert_eq!(kv(&fetched, "iskTaxRate").as_f64(), Some(0.05));
    assert_eq!(kv(&fetched, "loyaltyPointTaxRate").as_f64(), Some(0.22));
    assert_eq!(kv(&fetched, "friendlyFire").as_bool(), Some(true));
    assert_eq!(kv(&fetched, "typeface").as_str(), Some("Futura"));

    // V24 15-arg AddCorporation with args[2] = PyRep::None must STILL parse args[3..=14] and enforce string bounds.
    let f2_id = 90_000_910_i64;
    let f2_uid = f2_id as u64;
    let mut founder2 = founder.clone();
    founder2.session_id = 910;
    founder2.character_id = Some(f2_id);
    founder2.user_id = Some(f2_uid);
    founder2.corporation_id = Some(1_000_170);
    founder2.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, f2_id, "Founder910", 1_000_170, 30_000_142, 55_777_910);
        ch.user_id = f2_uid;
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(5_000_000);
        snap.characters.insert(f2_id, ch);
        snap.skills.entry(f2_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                skill_points: 100_000.0,
                level: 2,
            },
        );
        let uname = format!("u{f2_uid}");
        snap.accounts.insert(
            uname.clone(),
            eve_db::AccountRecord {
                user_id: f2_uid,
                username: uname,
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(f2_uid, founder2.session_id, Some(f2_id), true, 1_700_000_000)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(f2_id);

    let err_over_name = call_gov(
        &ctx,
        &mut founder2,
        "corpRegistry",
        "AddCorporation",
        vec![PyRep::String("N".repeat(101)), PyRep::String("OVRN".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_over_name.code, "InvalidArgument");

    let err_over_tf = call_gov(
        &ctx,
        &mut founder2,
        "corpRegistry",
        "AddCorporation",
        vec![
            PyRep::String("Bound TF Corp".into()),
            PyRep::String("BTFC".into()),
            PyRep::None,
            PyRep::String("https://ok.example".into()),
            PyRep::Real(0.10),
            PyRep::Integer(415),
            PyRep::Integer(416),
            PyRep::Integer(417),
            PyRep::Integer(671),
            PyRep::Integer(672),
            PyRep::Integer(673),
            PyRep::String("F".repeat(65)),
            PyRep::Bool(true),
            PyRep::Bool(false),
            PyRep::Real(0.10),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_over_tf.code, "InvalidArgument");

    let created_none_desc = call_gov(
        &ctx,
        &mut founder2,
        "corpRegistry",
        "AddCorporation",
        vec![
            PyRep::String("None Desc Co".into()),
            PyRep::String("NDCO".into()),
            PyRep::None,
            PyRep::String("https://none-desc.example".into()),
            PyRep::Real(0.12),
            PyRep::Integer(415),
            PyRep::Integer(416),
            PyRep::Integer(417),
            PyRep::Integer(671),
            PyRep::Integer(672),
            PyRep::Integer(673),
            PyRep::String("Arial".into()),
            PyRep::Bool(false),
            PyRep::Bool(true),
            PyRep::Real(0.18),
        ],
    )
    .await
    .unwrap();
    let none_desc_cid = kv(&created_none_desc, "corporationID").as_i64().unwrap();
    let fetched2 = call_gov(&ctx, &mut founder2, "corpRegistry", "GetCorporation", vec![])
        .await
        .unwrap();
    assert_eq!(kv(&fetched2, "corporationID").as_i64(), Some(none_desc_cid));
    assert_eq!(kv(&fetched2, "description").as_str(), Some(""));
    assert_eq!(kv(&fetched2, "url").as_str(), Some("https://none-desc.example"));
    assert_eq!(kv(&fetched2, "iskTaxRate").as_f64(), Some(0.12));
    assert_eq!(kv(&fetched2, "loyaltyPointTaxRate").as_f64(), Some(0.18));
    assert_eq!(kv(&fetched2, "friendlyFire").as_bool(), Some(true));
    assert_eq!(kv(&fetched2, "applicationsEnabled").as_bool(), Some(false));
    assert_eq!(kv(&fetched2, "typeface").as_str(), Some("Arial"));
}

#[tokio::test]
async fn title_parsers_delayed_grants_role_drop_and_kick_blocker_boundaries() {
    // 1. parse_title_row_cells boundaries (tid <= 0, reordered columns, ROLE_DIRECTOR stripping across all 8 masks)
    assert!(parse_title_row_cells(None, &[PyRep::Integer(0)]).is_none());
    assert!(parse_title_row_cells(None, &[PyRep::Integer(-1)]).is_none());
    let dir_plus = (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER) as i64;
    let default_cells = vec![
        PyRep::Integer(64),
        PyRep::String("Quartermaster".into()),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
    ];
    let (tid, tname, masks) = parse_title_row_cells(None, &default_cells).unwrap();
    assert_eq!(tid, 64);
    assert_eq!(tname, "Quartermaster");
    assert_eq!(masks, [corp_roles::ROLE_STATION_MANAGER; 8]);

    let custom_cols = vec!["titleName".to_string(), "titleID".to_string(), "rolesAtOther".to_string()];
    let reordered = vec![
        PyRep::String("Outpost Lead".into()),
        PyRep::Integer(128),
        PyRep::Integer(dir_plus),
    ];
    let (rtid, rtname, rmasks) = parse_title_row_cells(Some(&custom_cols), &reordered).unwrap();
    assert_eq!(rtid, 128);
    assert_eq!(rtname, "Outpost Lead");
    assert_eq!(rmasks[6], corp_roles::ROLE_STATION_MANAGER);

    // 2. parse_title_dict_entry boundaries (tid <= 0, all 8 role columns)
    assert!(parse_title_dict_entry(&PyRep::keyval([("titleID", PyRep::Integer(0))])).is_none());
    assert!(parse_title_dict_entry(&PyRep::keyval([("titleID", PyRep::Integer(-5))])).is_none());
    let dict_entry = PyRep::Dict(vec![
        (PyRep::String("titleID".into()), PyRep::Integer(256)),
        (PyRep::String("titleName".into()), PyRep::String("Treasurer".into())),
        (PyRep::String("roles".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("grantableRoles".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("rolesAtHQ".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("grantableRolesAtHQ".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("rolesAtBase".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("grantableRolesAtBase".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("rolesAtOther".into()), PyRep::Integer(dir_plus)),
        (PyRep::String("grantableRolesAtOther".into()), PyRep::Integer(dir_plus)),
    ]);
    let (dtid, dtname, dmasks) = parse_title_dict_entry(&dict_entry).unwrap();
    assert_eq!(dtid, 256);
    assert_eq!(dtname, "Treasurer");
    assert_eq!(dmasks, [corp_roles::ROLE_STATION_MANAGER; 8]);

    // 3. parse_title_update_rows across Object(header/lines), Object(KeyVal), List(cells), List(dict), Dict, and positional fallback
    let obj_rowset = PyRep::Object {
        type_name: "util.Rowset".into(),
        args: Box::new(PyRep::Dict(vec![
            (
                PyRep::String("header".into()),
                PyRep::List(vec![PyRep::String("titleID".into()), PyRep::String("titleName".into())]),
            ),
            (
                PyRep::String("lines".into()),
                PyRep::List(vec![
                    PyRep::List(vec![PyRep::Integer(512), PyRep::String("ObjRow".into())]),
                    dict_entry.clone(),
                ]),
            ),
        ])),
    };
    let parsed_obj_rs = parse_title_update_rows(&[obj_rowset]);
    assert_eq!(parsed_obj_rs.len(), 2);
    assert_eq!(parsed_obj_rs[0].0, 512);
    assert_eq!(parsed_obj_rs[1].0, 256);

    let parsed_kv = parse_title_update_rows(&[PyRep::keyval([
        ("titleID", PyRep::Integer(1024)),
        ("titleName", PyRep::String("KVTitle".into())),
    ])]);
    assert_eq!(parsed_kv[0].0, 1024);

    let parsed_list = parse_title_update_rows(&[PyRep::List(vec![PyRep::List(default_cells.clone()), dict_entry.clone()])]);
    assert_eq!(parsed_list.len(), 2);
    assert_eq!(parsed_list[0].0, 64);
    assert_eq!(parsed_list[1].0, 256);

    let parsed_dict = parse_title_update_rows(&[dict_entry]);
    assert_eq!(parsed_dict[0].0, 256);

    let parsed_pos = parse_title_update_rows(&[
        PyRep::Integer(0),
        PyRep::String("FallbackPos".into()),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
        PyRep::Integer(dir_plus),
    ]);
    assert_eq!(parsed_pos[0].0, 1);
    assert_eq!(parsed_pos[0].1, "FallbackPos");
    assert_eq!(parsed_pos[0].2, [corp_roles::ROLE_STATION_MANAGER; 8]);

    // 4. Delayed division role grant, role drop timestamp on UpdateTitle & DeleteTitle, and title_mask_grants_roles
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, _member) = seed_player_corps(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let g = snap.corp_governance.entry(98_000_001).or_default();
        g.member_titles.insert(90_000_002, 16);
    });
    let delayed_role = corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK
        & !(corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK - 1);
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitle",
        vec![
            PyRep::Integer(16),
            PyRep::String("Hangar Chief".into()),
            PyRep::Integer(delayed_role as i64),
        ],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let g = &snap.corp_governance[&98_000_001];
        assert_eq!(
            g.pending_title_role_grants.get(&90_000_002).copied(),
            Some((delayed_role, 1_700_000_000 + 86_400))
        );
        assert!(crate::services::fleet_social::fleet_social_corp_tail::title_mask_grants_roles(
            snap, 98_000_001, 16
        ));
        assert!(!crate::services::fleet_social::fleet_social_corp_tail::title_mask_grants_roles(
            snap, 98_000_001, 0
        ));
    });

    // Dropping the role records role_drop_timestamps
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitle",
        vec![
            PyRep::Integer(16),
            PyRep::String("Hangar Chief".into()),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corp_governance[&98_000_001]
                .role_drop_timestamps
                .get(&90_000_002)
                .copied(),
            Some(1_700_000_000)
        );
    });
}

#[tokio::test]
async fn add_corp_and_recruitment_ads_store_typed_fields_with_recruiter_cap_and_zero_service_blobs() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, _mem) = seed_player_corps(&ctx);
    let corp_id = ceo.corporation_id.unwrap();
    let ceo_id = ceo.character_id.unwrap();
    let extra_members: Vec<i64> = (90_000_501..=90_000_507).collect();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            for &mid in &extra_members {
                corp.members.insert(mid);
            }
        }
    });

    // 1. parse_recruiter_ids / CreateRecruitmentAd deduplicates with order preservation (7 items -> 6 unique)
    // and enforces V24 corporationMaxCorpRecruiters = 6 on both CreateRecruitmentAd and UpdateRecruitmentAd.
    let rec_input_6_dedup = vec![
        ceo_id,
        extra_members[0],
        ceo_id,
        extra_members[1],
        extra_members[2],
        extra_members[0],
        extra_members[3],
        extra_members[4],
    ];
    let expected_6 = vec![
        ceo_id,
        extra_members[0],
        extra_members[1],
        extra_members[2],
        extra_members[3],
        extra_members[4],
    ];
    let created = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(14),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Dedup 6".into()),
            PyRep::List(rec_input_6_dedup.iter().copied().map(PyRep::Integer).collect()),
            PyRep::String("Six Recruiters".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    let ad_id = kv(&created, "adID").as_i64().unwrap();
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(ad_id)],
        )
        .await
        .unwrap(),
        PyRep::List(expected_6.iter().copied().map(PyRep::Integer).collect())
    );

    // 7 distinct corp members must fail with InvalidArgument on both Create and Update without debiting ISK.
    let bal_before_cap = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1));
    let over_7: Vec<PyRep> = std::iter::once(ceo_id)
        .chain(extra_members[..6].iter().copied())
        .map(PyRep::Integer)
        .collect();
    let err_create_over = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::Integer(14),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Too many".into()),
            PyRep::List(over_7.clone()),
            PyRep::String("Over 6 Create".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_create_over.code, "InvalidArgument");

    let err_update_over = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Too many update".into()),
            PyRep::List(over_7),
            PyRep::String("Over 6 Update".into()),
            PyRep::Integer(7),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_update_over.code, "InvalidArgument");

    let spam_65 = vec![PyRep::Integer(ceo_id); 65];
    let err_spam = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Spam input".into()),
            PyRep::List(spam_65),
            PyRep::String("Spam Cap".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_spam.code, "InvalidArgument");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1)),
        bal_before_cap
    );

    // UpdateRecruitmentAd with reversed deduplicated 2 recruiters preserves new order on CorpRecruitmentAd.
    call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "UpdateRecruitmentAd",
        vec![
            PyRep::Integer(ad_id),
            PyRep::Integer(7),
            PyRep::Integer(1),
            PyRep::String("Updated list".into()),
            PyRep::List(vec![
                PyRep::Integer(extra_members[2]),
                PyRep::Integer(extra_members[2]),
                PyRep::Integer(ceo_id),
            ]),
            PyRep::String("Updated Two".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(ad_id)],
        )
        .await
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(extra_members[2]), PyRep::Integer(ceo_id)])
    );

    // 2. Create >130 player corporations + recruitment ads (>256 keys under old blob scheme)
    // with distinct explicit LP tax (including 0.0 when ISK tax != 0.0) and both string/int typefaces.
    let mut created_corps = Vec::with_capacity(135);
    let mut last_founder = None;
    let mut last_ad_id = 0_i64;
    for i in 0..135_i64 {
        let cid_char = 90_700_000 + i;
        let uid = cid_char as u64;
        let mut sess = SessionState::new(700 + i as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid_char);
        sess.corporation_id = Some(1_000_170);
        sess.station_id = Some(60_003_760);

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = clone_char_with(
                snap,
                cid_char,
                &format!("Founder{i}"),
                1_000_170,
                30_000_142,
                55_800_000 + i,
            );
            ch.user_id = uid;
            ch.station_id = Some(60_003_760);
            ch.balance = IskCents::from_isk(5_000_000);
            snap.characters.insert(cid_char, ch);
            snap.skills.entry(cid_char).or_default().insert(
                3363,
                TrainedSkillRecord {
                    type_id: 3363,
                    skill_points: 100_000.0,
                    level: 2,
                },
            );
            let uname = format!("scale_u{uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        });
        ctx.db
            .acquire_online_lock(uid, sess.session_id, Some(cid_char), true, 1_700_000_000)
            .unwrap();

        let tf_rep = if i % 2 == 0 {
            PyRep::String(format!("Typeface{i}"))
        } else {
            PyRep::Integer(i + 10)
        };
        let lp_tax = if i == 134 { 0.0 } else { 0.18 };
        let res = call_gov(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            vec![
                PyRep::String(format!("Scale Corp {i}")),
                PyRep::String(format!("S{i:03}")),
                PyRep::String("Desc".into()),
                PyRep::String("https://scale.example".into()),
                PyRep::Real(0.15),
                PyRep::Integer(415),
                PyRep::Integer(416),
                PyRep::Integer(417),
                PyRep::Integer(671),
                PyRep::Integer(672),
                PyRep::Integer(673),
                tf_rep,
                PyRep::Bool(true),
                PyRep::Bool(false),
                PyRep::Real(lp_tax),
            ],
        )
        .await
        .unwrap();
        let new_cid = kv(&res, "corporationID").as_i64().unwrap();
        created_corps.push(new_cid);

        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations
                .get_mut(&new_cid)
                .unwrap()
                .set_division_balance(1, IskCents::from_isk(1_000_000));
        });
        let ad = call_gov(
            &ctx,
            &mut sess,
            "corpRecProxy",
            "CreateRecruitmentAd",
            vec![
                PyRep::Integer(14),
                PyRep::Integer(7),
                PyRep::Integer(1),
                PyRep::String(format!("Scale Ad {i}")),
                PyRep::List(vec![PyRep::Integer(cid_char), PyRep::Integer(cid_char)]),
                PyRep::String(format!("Title {i}")),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap();
        last_ad_id = kv(&ad, "adID").as_i64().unwrap();
        last_founder = Some(sess);
    }

    ctx.db.with_snapshot(|snap| {
        let per_entity_keys: Vec<&String> = snap
            .service_state_blobs
            .keys()
            .filter(|k| *k != "__server_now_unix")
            .collect();
        assert!(
            per_entity_keys.is_empty(),
            "AddCorporation and CreateRecruitmentAd/UpdateRecruitmentAd must not add per-entity keys to service_state_blobs (got {per_entity_keys:?})"
        );
    });

    // Save to SQLite, reopen, and verify explicit LP tax 0.0, string typeface, integer typeface, and ad recruiters survive reload.
    let db_path = std::env::temp_dir().join(format!(
        "evomu_corp_scale_roundtrip_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);
    ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let reopened_repo = std::sync::Arc::new(
        eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"),
    );
    let reopened_ctx = crate::router::ServerContext::with_sde_and_repository(
        ctx.sde.clone(),
        reopened_repo,
    );
    reopened_ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_700_000_000,
    )));

    let mut sess_last = last_founder.unwrap();
    let corp_last_info = call_gov(
        &reopened_ctx,
        &mut sess_last,
        "corpRegistry",
        "GetCorporation",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(kv(&corp_last_info, "corporationID").as_i64(), Some(created_corps[134]));
    assert_eq!(kv(&corp_last_info, "iskTaxRate").as_f64(), Some(0.15));
    assert_eq!(kv(&corp_last_info, "loyaltyPointTaxRate").as_f64(), Some(0.0));
    assert_eq!(kv(&corp_last_info, "typeface").as_str(), Some("Typeface134"));

    let corp_int_tf = call_gov(
        &reopened_ctx,
        &mut sess_last,
        "corpmgr",
        "GetPublicInfo",
        vec![PyRep::Integer(created_corps[133])],
    )
    .await
    .unwrap();
    assert_eq!(
        corp_int_tf.dict_get("typeface").and_then(|v| v.as_i64()),
        Some(143)
    );
    assert_eq!(
        call_gov(
            &reopened_ctx,
            &mut sess_last,
            "corpRecProxy",
            "GetRecruiters",
            vec![PyRep::Integer(last_ad_id)],
        )
        .await
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(sess_last.character_id.unwrap())])
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));
}

#[tokio::test]
async fn corp_gov_resolves_db_first_corp_id_over_stale_session() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_a = 98_700_301_i64;
    let corp_b = 98_700_302_i64;
    let ceo_a = 91_700_301_i64;
    let ceo_b = 91_700_302_i64;
    let acc_b = 91_700_303_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ceo, name, extra_mem) in [
            (corp_a, ceo_a, "GovCorpA", None),
            (corp_b, ceo_b, "GovCorpB", Some(acc_b)),
        ] {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = cid;
            corp.name = name.into();
            corp.ticker = name[..3].to_uppercase();
            corp.ceo_id = ceo;
            corp.members.insert(ceo);
            if let Some(mid) = extra_mem {
                corp.members.insert(mid);
                corp.member_roles
                    .insert(mid, eve_db::corp_roles::ROLE_ACCOUNTANT);
            }
            corp.ensure_wallet_divisions();
            snap.corporations.insert(cid, corp);
        }
        for (char_id, cid) in [(ceo_a, corp_a), (ceo_b, corp_b), (acc_b, corp_b)] {
            snap.characters.insert(
                char_id,
                eve_db::CharacterRecord {
                    character_id: char_id,
                    corporation_id: cid,
                    ..Default::default()
                },
            );
        }
    });

    // ceo_b has DB corp_b while session.corporation_id is stale corp_a -> UpdateCorporationAbilities/UpdateAttributes & GetWalletDivisionsInfo target DB corp_b.
    let mut stale_b_sess = SessionState::new(91_702);
    stale_b_sess.user_id = Some(702);
    stale_b_sess.character_id = Some(ceo_b);
    stale_b_sess.corporation_id = Some(corp_a);
    stale_b_sess.corp_role = 0;

    let updated_b = call_gov(
        &ctx,
        &mut stale_b_sess,
        "corpRegistry",
        "UpdateCorporationAbilities",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(kv(&updated_b, "corporationID").as_i64(), Some(corp_b));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.corp_member_limits.contains_key(&corp_a));
        assert!(snap.corp_member_limits.contains_key(&corp_b));
    });

    // GetWalletDivisionsInfo with omitted args also resolves DB-first corp_b instead of stale session corp_a.
    let mut acc_stale_sess = SessionState::new(91_703);
    acc_stale_sess.user_id = Some(703);
    acc_stale_sess.character_id = Some(acc_b);
    acc_stale_sess.corporation_id = Some(corp_a);
    acc_stale_sess.corp_role = u64::MAX;
    let divs = call_gov(
        &ctx,
        &mut acc_stale_sess,
        "account",
        "GetWalletDivisionsInfo",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&divs).len(), 7);
}

#[tokio::test]
async fn corp_gov_enforces_npc_corp_excluded_member_and_cross_corp_wallet_guards() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_a = 98_700_401_i64;
    let corp_b = 98_700_402_i64;
    let npc_corp = 1_000_009_i64;
    let ceo_a = 91_700_401_i64;
    let excluded_a = 91_700_402_i64;
    let cross_a = 91_700_403_i64;
    let npc_char = 91_700_404_i64;
    let gm_char = 91_700_405_i64;
    let ad_a_id = 7_401_i64;
    let ad_npc_id = 7_402_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ca = eve_db::CorporationRecord::default();
        ca.corporation_id = corp_a;
        ca.name = "GuardCorpA".into();
        ca.ticker = "GCA".into();
        ca.ceo_id = ceo_a;
        ca.members = std::collections::HashSet::from([ceo_a, cross_a]);
        ca.member_roles.insert(
            excluded_a,
            corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_ACCOUNTANT
                | corp_roles::ROLE_PERSONNEL_MANAGER
                | corp_roles::ROLE_DIPLOMAT,
        );
        ca.ensure_wallet_divisions();
        ca.set_division_balance(1, IskCents::from_isk(5_000_000));
        snap.corporations.insert(corp_a, ca);

        let mut cb = eve_db::CorporationRecord::default();
        cb.corporation_id = corp_b;
        cb.name = "GuardCorpB".into();
        cb.ticker = "GCB".into();
        cb.ceo_id = cross_a;
        cb.members = std::collections::HashSet::from([cross_a]);
        cb.member_roles
            .insert(cross_a, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT);
        cb.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, cb);

        let mut npc = eve_db::CorporationRecord::default();
        npc.corporation_id = npc_corp;
        npc.name = "SeededNpcCorp".into();
        npc.ticker = "SNPC".into();
        npc.ceo_id = npc_char;
        npc.members = std::collections::HashSet::from([npc_char]);
        npc.member_roles.insert(
            npc_char,
            corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_ACCOUNTANT
                | corp_roles::ROLE_PERSONNEL_MANAGER
                | corp_roles::ROLE_DIPLOMAT,
        );
        npc.ensure_wallet_divisions();
        npc.set_division_balance(1, IskCents::from_isk(5_000_000));
        snap.corporations.insert(npc_corp, npc);

        for (cid, coid) in [
            (ceo_a, corp_a),
            (excluded_a, corp_a),
            (cross_a, corp_a),
            (npc_char, npc_corp),
            (gm_char, corp_a),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: coid,
                    ..Default::default()
                },
            );
        }

        let gov_a = snap.corp_governance.entry(corp_a).or_default();
        gov_a.shares.insert(corp_a, 1_000);
        gov_a.company_shares = 1_000;
        let gov_npc = snap.corp_governance.entry(npc_corp).or_default();
        gov_npc.shares.insert(npc_corp, 1_000);
        gov_npc.company_shares = 1_000;

        for (ad_id, cid, rec) in [(ad_a_id, corp_a, ceo_a), (ad_npc_id, npc_corp, npc_char)] {
            snap.fleet_and_social_ops
                .corp_recruitment_ads
                .push(eve_db::CorpRecruitmentAd {
                    ad_id,
                    corp_id: cid,
                    title: "Existing Ad".into(),
                    description: "Desc".into(),
                    type_mask: 1,
                    recruiter_id: rec,
                    recruiters: vec![rec],
                    created_unix: 1_700_000_000,
                    expiry_unix: 1_700_000_000 + 86_400,
                    days: 1,
                    lang_mask: 1,
                });
        }
    });

    // 1. GetWalletDivisionsInfo: rejects NPC corp, excluded member, and explicit cross-corp args[0]; allows GM.
    let mut npc_sess = SessionState::new(91_744);
    npc_sess.user_id = Some(744);
    npc_sess.character_id = Some(npc_char);
    npc_sess.corporation_id = Some(npc_corp);
    npc_sess.corp_role = u64::MAX;
    assert_eq!(
        call_gov(
            &ctx,
            &mut npc_sess,
            "account",
            "GetWalletDivisionsInfo",
            vec![PyRep::Integer(npc_corp)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut excluded_sess = SessionState::new(91_742);
    excluded_sess.user_id = Some(742);
    excluded_sess.character_id = Some(excluded_a);
    excluded_sess.corporation_id = Some(corp_a);
    excluded_sess.corp_role = u64::MAX;
    assert_eq!(
        call_gov(
            &ctx,
            &mut excluded_sess,
            "account",
            "GetWalletDivisionsInfo",
            vec![PyRep::Integer(corp_a)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut cross_sess = SessionState::new(91_743);
    cross_sess.user_id = Some(743);
    cross_sess.character_id = Some(cross_a);
    cross_sess.corporation_id = Some(corp_b);
    cross_sess.corp_role = u64::MAX;
    assert_eq!(
        call_gov(
            &ctx,
            &mut cross_sess,
            "account",
            "GetWalletDivisionsInfo",
            vec![PyRep::Integer(corp_b)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut gm_sess = SessionState::new(91_745);
    gm_sess.user_id = Some(745);
    gm_sess.character_id = Some(gm_char);
    gm_sess.corporation_id = Some(corp_a);
    gm_sess.role |= eve_machonet::service_roles::ROLE_GMH;
    let gm_divs = call_gov(
        &ctx,
        &mut gm_sess,
        "account",
        "GetWalletDivisionsInfo",
        vec![PyRep::Integer(corp_b)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&gm_divs).len(), 7);

    // 2. handle_corp_gov + PayoutDividend + recruitment ads + GetSharesByShareholder reject both NPC corp and excluded member.
    for (label, sess, cid, ad_id, recipient) in [
        ("npc_corp", &mut npc_sess, npc_corp, ad_npc_id, npc_char),
        ("excluded_member", &mut excluded_sess, corp_a, ad_a_id, ceo_a),
    ] {
        for (svc, method, args, expected_code) in [
            (
                "corpRegistry",
                "MoveCompanyShares",
                vec![PyRep::Integer(cid), PyRep::Integer(recipient), PyRep::Integer(10)],
                "CorpRoleDenied",
            ),
            (
                "corpRegistry",
                "UpdateTitle",
                vec![PyRep::Integer(1), PyRep::String("Denied".into()), PyRep::Integer(0)],
                "CorpRoleDenied",
            ),
            (
                "corpRegistry",
                "CreateLabel",
                vec![PyRep::String("DeniedLabel".into()), PyRep::Integer(0x00FF00)],
                "CorpRoleDenied",
            ),
            (
                "corpRegistry",
                "PayoutDividend",
                vec![PyRep::Bool(false), PyRep::Real(100_000.0), PyRep::Integer(1)],
                "CorpRoleDenied",
            ),
            (
                "corpRecProxy",
                "CreateRecruitmentAd",
                vec![
                    PyRep::String("Denied Ad".into()),
                    PyRep::String("Should fail".into()),
                    PyRep::Integer(1),
                    PyRep::Integer(7),
                ],
                "CorpRoleDenied",
            ),
            (
                "corpRecProxy",
                "UpdateRecruitmentAd",
                vec![
                    PyRep::Integer(ad_id),
                    PyRep::Integer(1),
                    PyRep::String("Denied Update".into()),
                    PyRep::String("Should fail".into()),
                ],
                "CorpRoleDenied",
            ),
            (
                "corpRecProxy",
                "DeleteRecruitmentAd",
                vec![PyRep::Integer(ad_id)],
                "CorpRoleDenied",
            ),
            (
                "corpRegistry",
                "GetSharesByShareholder",
                vec![PyRep::Bool(true)],
                "CrpAccessDenied",
            ),
        ] {
            let err = call_gov(&ctx, sess, svc, method, args).await.unwrap_err();
            assert_eq!(
                err.code, expected_code,
                "{label}: {svc}.{method} must fail with {expected_code}"
            );
        }
    }
}
