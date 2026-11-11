//! Additional edge-case and branch-coverage unit tests for `corp_registry_ops.rs`.

#![forbid(unsafe_code)]

use crate::router::BoundObjectEntry;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item;
use crate::services::corp_registry_ops::*;
use eve_db::snapshot_ext::MarketOrderMeta;
use eve_db::{
    CharacterLoginStats, CorporationRecord, InventoryFlag, IskCents, MarketOrderRecord,
    VirtualServerClock,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

use crate::services::corp_registry_ops_extra_unit_tests::{call_corp_reg, setup_ctx};

#[tokio::test]
async fn dispatch_and_invbroker_edge_cases_and_structure_ownership() {
    let (ctx, mut sess, ceo_id, _ship_id, corp_id, station_id) = setup_ctx();
    let kw = HashMap::new();

    // Non-matching service / sibling corpRegistry method return None
    assert!(handle(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[], &kw).is_none());
    assert!(handle(&ctx, &mut sess, "corpRegistry", "GetCorporation", &[], &kw).is_none());
    assert!(handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "charMgr",
        "GetCharacterInfo",
        &[],
        &kw
    )
    .await
    .is_none());

    // invbroker.List bound to a structure not matching session.structure_id -> BoundObjectOwnershipViolation
    let structure_id = 1_000_000_000_555_i64;
    let bound_struct = BoundObjectEntry {
        bound_id: 99,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap(),
        character_id: ceo_id,
        service: "invbroker".into(),
        object_id: structure_id,
        oid_str: format!("N=1:{structure_id}"),
        ref_id: 1,
    };
    let err = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        Some(&bound_struct),
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::CorpSAG1 as i64)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "BoundObjectOwnershipViolation");

    // invbroker.List with flag outside 116..=122 returns None
    assert!(handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
    )
    .await
    .is_none());

    // invbroker.List resolving location via session.ship_id when unbound and station/structure cleared
    let ship_loc = sess.ship_id;
    sess.station_id = None;
    sess.structure_id = None;
    let list_res = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::CorpSAG1 as i64)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("List ok");
    assert!(matches!(list_res, PyRep::ObjectEx { .. }));
    sess.ship_id = ship_loc;
    sess.station_id = Some(station_id);

    // invbroker.Add: unauth session returns AuthRequired error
    let mut unauth = SessionState::default();
    let unauth_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut unauth,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert!(!unauth_err.code.is_empty());

    // invbroker.Add: item in hangar (non-CorpSAG) returns None; item in CorpSAG1 when caller IS CEO returns None (allowed)
    let normal_item_id = 777_001_i64;
    let corp_item_id = 777_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            normal_item_id,
            make_fighter_item(
                normal_item_id,
                34,
                ceo_id,
                station_id,
                InventoryFlag::Hangar,
                10,
            ),
        );
        snap.items.insert(
            corp_item_id,
            make_fighter_item(
                corp_item_id,
                34,
                corp_id,
                station_id,
                InventoryFlag::CorpSAG1,
                10,
            ),
        );
    });
    assert!(handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(normal_item_id)],
        &kw,
    )
    .await
    .is_none());
    assert!(handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(corp_item_id)],
        &kw,
    )
    .await
    .is_none());
}

#[test]
fn get_eve_owners_bloodlines_access_denied_and_name_suggestions() {
    let (ctx, mut sess, ceo_id, _ship_id, corp_id, _station_id) = setup_ctx();

    // Target corp mismatch -> CrpAccessDenied
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut sess,
            "GetEveOwners",
            &[PyRep::Integer(corp_id + 999)]
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Caller with corporation_id <= 0 -> CrpAccessDenied
    let mut no_corp_sess = sess.clone();
    no_corp_sess.corporation_id = Some(0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = 0;
    });
    assert_eq!(
        call_corp_reg(&ctx, &mut no_corp_sess, "GetEveOwners", &[])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = corp_id;
    });

    // Bloodlines across races + unknown bloodline fallback
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        let base = snap.characters[&ceo_id].clone();
        for (idx, bl) in [1u32, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 19, 999]
            .into_iter()
            .enumerate()
        {
            let cid = 90_100_000 + idx as i64;
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.bloodline_id = bl;
            ch.gender = if idx % 2 == 0 { 0 } else { 1 };
            snap.characters.insert(cid, ch);
            corp.members.insert(cid);
        }
    });
    let owners = call_corp_reg(&ctx, &mut sess, "GetEveOwners", &[PyRep::Integer(corp_id)])
        .expect("GetEveOwners ok");
    if let PyRep::Rowset { rows, .. } = owners {
        assert!(rows.len() >= 14);
    } else {
        panic!("expected Rowset from GetEveOwners");
    }

    // Fallback when corp record missing in snap.corporations (scans snap.characters)
    let orphan_corp_id = 98_999_111_i64;
    let mut orphan_sess = sess.clone();
    orphan_sess.corporation_id = Some(orphan_corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = orphan_corp_id;
    });
    let orphan_owners =
        call_corp_reg(&ctx, &mut orphan_sess, "GetEveOwners", &[]).expect("fallback GetEveOwners");
    assert!(matches!(orphan_owners, PyRep::Rowset { rows, .. } if !rows.is_empty()));

    // GetSuggestedAllianceShortNames: < 3 words, < 3 alphanumeric fallback "ALLI", default arg
    let short1 = call_corp_reg(
        &ctx,
        &mut sess,
        "GetSuggestedAllianceShortNames",
        &[PyRep::String("Red Dawn".into())],
    )
    .unwrap();
    assert!(matches!(short1, PyRep::List(l) if !l.is_empty()));
    let short_empty = call_corp_reg(
        &ctx,
        &mut sess,
        "GetSuggestedAllianceShortNames",
        &[PyRep::String("---".into())],
    )
    .unwrap();
    if let PyRep::List(items) = short_empty {
        assert_eq!(
            items.first(),
            Some(&PyRep::keyval([("shortName", PyRep::String("ALLI".into()))]))
        );
    } else {
        panic!("expected list");
    }
    assert!(matches!(
        call_corp_reg(&ctx, &mut sess, "GetSuggestedAllianceShortNames", &[]).unwrap(),
        PyRep::List(_)
    ));

    // GetSuggestedTickerNames: 1-char input + filtering taken corp ticker
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_888_001,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_888_001; __corp.name = "Alpha".into(); __corp.ticker = "ALPH".into(); __corp.ensure_wallet_divisions(); __corp },
        );
    });
    let tickers = call_corp_reg(
        &ctx,
        &mut sess,
        "GetSuggestedTickerNames",
        &[PyRep::String("Alpha Beta Gamma".into())],
    )
    .unwrap();
    if let PyRep::List(items) = tickers {
        assert!(!items.contains(&PyRep::String("ALPH".into())));
    } else {
        panic!("expected ticker list");
    }
    let tiny_ticker = call_corp_reg(
        &ctx,
        &mut sess,
        "GetSuggestedTickerNames",
        &[PyRep::String("X".into())],
    )
    .unwrap();
    assert!(matches!(tiny_ticker, PyRep::List(_)));
}

#[test]
fn set_account_key_non_corp_and_execute_actions_role_edge_paths() {
    let (ctx, mut sess, ceo_id, _ship_id, corp_id, _station_id) = setup_ctx();
    let member_id = 90_000_002_i64;
    let kw = HashMap::new();

    // SetAccountKey: 1000 -> 1000 yields no OnSessionChanged
    sess.corp_account_key = 1000;
    let _ = call_corp_reg(&ctx, &mut sess, "SetAccountKey", &[PyRep::Integer(1000)]).unwrap();
    assert!(ctx.drain_notifications_for_char(ceo_id).is_empty());

    // Non-corp (missing from snap.corporations) allows 1000 and denies 1001
    let mut npc_sess = sess.clone();
    npc_sess.corporation_id = Some(1_000_125);
    assert!(call_corp_reg(
        &ctx,
        &mut npc_sess,
        "SetAccountKey",
        &[PyRep::Integer(1000)]
    )
    .is_ok());
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut npc_sess,
            "SetAccountKey",
            &[PyRep::Integer(1001)]
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // ExecuteActions validation errors: non-Tuple/List action, unknown verb, negative value, non-CEO targeting CEO
    let bad_shape = call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Integer(42)]),
        ],
    );
    assert_eq!(bad_shape.unwrap_err().code, "InvalidParameter");

    let bad_verb = call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("bogusVerb".into()),
                PyRep::Integer(1),
            ])]),
        ],
    );
    assert_eq!(bad_verb.unwrap_err().code, "InvalidParameter");

    let neg_val = call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(-1),
            ])]),
        ],
    );
    assert_eq!(neg_val.unwrap_err().code, "CrpAccessDenied");

    // Grant member PersonnelManager + Director? No, Director cannot touch CEO: give member Director role & try targeting CEO
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    let mut dir_sess = sess.clone();
    dir_sess.character_id = Some(member_id);
    dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let hit_ceo_err = call_corp_reg(
        &ctx,
        &mut dir_sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(ceo_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(0),
            ])]),
        ],
    );
    assert_eq!(hit_ceo_err.unwrap_err().code, "CrpAccessDenied");

    // Title director-bit protection for non-CEO director + legacy SetTitle errors + SetFriendlyFire Integer
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.titles.insert(
            1,
            ("DirectorTitle".into(), eve_db::corp_roles::ROLE_DIRECTOR),
        );
        gov.titles.insert(
            2,
            (
                "HangarTitle".into(),
                eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK,
            ),
        );
    });
    let title_dir_err = call_corp_reg(
        &ctx,
        &mut dir_sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("add".into()),
                PyRep::String("titleMask".into()),
                PyRep::Integer(1),
            ])]),
        ],
    );
    assert_eq!(title_dir_err.unwrap_err().code, "CrpAccessDenied");

    // CEO clears baseID via 0, sets accountKey outside 1000..=1006 (ignored), exercises unknown prop,
    // and tests legacy SetTitle (negative mask, non-member, director guard, SetNote non-member, SetFriendlyFire Integer)
    call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("baseID".into()),
                    PyRep::Integer(60_003_760),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("baseID".into()),
                    PyRep::Integer(0),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("accountKey".into()),
                    PyRep::Integer(9999),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("divisionID".into()),
                    PyRep::Integer(2),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("squadronID".into()),
                    PyRep::Integer(3),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("ignoredUnknown".into()),
                    PyRep::Integer(1),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("add".into()),
                    PyRep::String("titleMask".into()),
                    PyRep::Integer(2),
                ]),
            ]),
        ],
    )
    .expect("batch ExecuteActions ok");

    // Verify base removed and pending_title_role_grants inserted; then revoke titleMask -> cleared + drop timestamp recorded
    ctx.db.with_snapshot(|snap| {
        let g = &snap.corp_governance[&corp_id];
        assert!(!g.member_bases.contains_key(&member_id));
        assert!(g.pending_title_role_grants.contains_key(&member_id));
    });
    call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("remove".into()),
                PyRep::String("titleMask".into()),
                PyRep::Integer(2),
            ])]),
        ],
    )
    .expect("remove titleMask ok");
    ctx.db.with_snapshot(|snap| {
        let g = &snap.corp_governance[&corp_id];
        assert!(!g.pending_title_role_grants.contains_key(&member_id));
        assert!(g.role_drop_timestamps.contains_key(&member_id));
    });

    // Legacy SetFriendlyFire with Integer(1) and SetTitle error paths
    assert!(call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetFriendlyFire".into()),
                PyRep::Integer(1),
            ])]),
        ],
    )
    .is_ok());
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut sess,
            "ExecuteActions",
            &[
                PyRep::Integer(corp_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("SetTitle".into()),
                    PyRep::Integer(member_id),
                    PyRep::Integer(-5),
                ])]),
            ],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut sess,
            "ExecuteActions",
            &[
                PyRep::Integer(corp_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("SetNote".into()),
                    PyRep::Integer(99_999_999),
                    PyRep::String("hi".into()),
                ])]),
            ],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Bind default + Unbind errors and zero bound_id no-op
    let bound = call_corp_reg(&ctx, &mut sess, "Bind", &[]).unwrap();
    let bound_id = bound.as_i64().unwrap();
    assert!(bound_id > 0);
    assert_eq!(
        ctx.bound_objects
            .read()
            .unwrap()
            .get(&(bound_id as u64))
            .map(|e| e.object_id),
        Some(corp_id)
    );
    assert_eq!(
        call_corp_reg(&ctx, &mut sess, "Unbind", &[PyRep::Integer(0)]).unwrap(),
        PyRep::None
    );
    assert!(call_corp_reg(&ctx, &mut sess, "UnBind", &[PyRep::Integer(999_999)]).is_err());

    let mut unauth = SessionState::default();
    assert!(
        handle(&ctx, &mut unauth, "corpRegistry", "Unbind", &[], &kw)
            .unwrap()
            .is_err()
    );
}

#[test]
fn market_info_info_window_get_member_and_member_tracking_edge_cases() {
    let (ctx, mut sess, ceo_id, _ship_id, corp_id, station_id) = setup_ctx();
    let member_id = 90_000_002_i64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000));
    ctx.set_clock(clock.clone());
    let now = ctx.now_unix();

    // Seed market orders (including vol_remaining == 0, multi sell/buy to test min/max)
    ctx.db.with_mut_snapshot(|snap| {
        for (oid, price_isk, vol_rem, is_bid) in [
            (1001_i64, 500_i64, 0_i32, false),
            (1002_i64, 200_i64, 10_i32, false),
            (1003_i64, 150_i64, 10_i32, false),
            (1004_i64, 90_i64, 10_i32, true),
            (1005_i64, 120_i64, 10_i32, true),
        ] {
            snap.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: corp_id,
                    type_id: 34,
                    station_id,
                    region_id: 10_000_002,
                    price: IskCents::from_isk(price_isk),
                    vol_entered: 100,
                    vol_remaining: vol_rem,
                    min_volume: 1,
                    is_bid,
                    is_npc: false,
                    escrow: IskCents(0),
                },
            );
            snap.market_order_meta.insert(
                oid,
                MarketOrderMeta {
                    is_corporation: true,
                    issued_unix: now,
                    duration_days: 90,
                    range: "0".into(),
                    wallet_division: Some(1),
                    issued_by: Some(ceo_id),
                    state: Some("open".into()),
                },
            );
        }
        // Set login stats for member (logged out 2h ago)
        snap.character_login_stats.insert(
            member_id,
            CharacterLoginStats {
                last_login_unix: now - 10_000,
                last_logout_unix: now - 7_200,
                ..Default::default()
            },
        );
        // Due pending role grant on member
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants
            .insert(member_id, (1u64 << 13, now - 10));
        gov.titles.insert(1, ("Quartermaster".into(), 0));
        gov.member_titles.insert(ceo_id, 1);
    });

    // handle_get_corp_market_info: CEO gets min sell 150.0, max buy 120.0
    let market = handle_get_corp_market_info(&ctx, &mut sess, &[])
        .expect("handled")
        .expect("market ok");
    if let PyRep::Rowset { rows, .. } = market {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1], PyRep::Real(150.0));
        assert_eq!(rows[0][5], PyRep::Real(120.0));
    } else {
        panic!("expected Rowset from GetCorpMarketInfo");
    }

    // Non-trader member denied
    let mut mem_sess = sess.clone();
    mem_sess.character_id = Some(member_id);
    mem_sess.corp_role = 0;
    assert_eq!(
        handle_get_corp_market_info(&ctx, &mut mem_sess, &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // GetInfoWindowDataForChar: passing corp_id overrides to corp with caller_id, + unknown char error
    let info_corp =
        handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
            .unwrap()
            .unwrap();
    assert_eq!(
        info_corp.dict_get("corporationID"),
        Some(&PyRep::Integer(corp_id))
    );
    assert_eq!(
        info_corp.dict_get("title"),
        Some(&PyRep::String("Quartermaster".into()))
    );
    assert_eq!(
        handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(99_999_888)])
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // GetMember: returns None for corp IDs (>=98M or NPC 1M..2M), NotCorpMember for outsider, NPC corp branch
    assert!(handle_get_member(&ctx, &mut sess, &[PyRep::Integer(corp_id)]).is_none());
    assert!(handle_get_member(&ctx, &mut sess, &[PyRep::Integer(1_000_125)]).is_none());
    assert_eq!(
        handle_get_member(&ctx, &mut sess, &[PyRep::Integer(90_999_888)])
            .unwrap()
            .unwrap_err()
            .code,
        "NotCorpMember"
    );
    let mut npc_mem_sess = sess.clone();
    npc_mem_sess.corporation_id = Some(1_000_125);
    assert_eq!(
        handle_get_member(&ctx, &mut npc_mem_sess, &[PyRep::Integer(90_999_888)])
            .unwrap()
            .unwrap_err()
            .code,
        "NotCorpMember"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = 1_000_125;
    });
    let npc_self = handle_get_member(&ctx, &mut npc_mem_sess, &[PyRep::Integer(ceo_id)])
        .unwrap()
        .expect("NPC self member");
    assert_eq!(
        npc_self.dict_get("corporationID"),
        Some(&PyRep::Integer(1_000_125))
    );

    // GetMemberTrackingInfo: filter_char_id + GetMemberTrackingInfoSimple non-director hides loc/ship
    let filtered = handle_get_member_tracking_info(
        &ctx,
        &mut sess,
        "GetMemberTrackingInfo",
        &[PyRep::Integer(member_id)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::Rowset { rows, .. } = filtered {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected Rowset");
    }
    let denied = handle_get_member_tracking_info(&ctx, &mut mem_sess, "GetMemberTrackingInfo", &[])
        .unwrap()
        .unwrap_err();
    assert_eq!(denied.code, "CorpRoleDenied");

    let simple_mem = handle_get_member_tracking_info(
        &ctx,
        &mut mem_sess,
        "GetMemberTrackingInfoSimple",
        &[PyRep::Integer(member_id)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::Rowset { rows, .. } = simple_mem {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][7], PyRep::None); // locationID hidden
        assert_eq!(rows[0][8], PyRep::None); // shipTypeID hidden
    } else {
        panic!("expected Rowset");
    }
}

#[test]
fn corp_registry_ops_remaining_mutant_killers() {
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use eve_db::corp_roles;

    let (ctx, mut sess, ceo_id, _ship_id, corp_id, _station_id) = setup_ctx();
    let orig_ship_id = sess.ship_id.expect("pilot has ship");
    let member_id = 90_000_002_i64;
    let member_b = 90_000_003_i64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000));
    ctx.set_clock(clock);
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock_with_computer_hash(
            sess.user_id.unwrap(),
            sess.session_id,
            Some(ceo_id),
            None,
            true,
            now,
        )
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut mb) = snap.characters.get(&member_id).cloned() {
            mb.character_id = member_b;
            mb.name = "Director Beta".into();
            snap.characters.insert(member_b, mb);
        }
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(member_b);
        corp.member_roles.insert(member_b, corp_roles::ROLE_DIRECTOR);
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.titles.insert(
            1,
            ("ExecDir".into(), corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_HANGAR_CAN_TAKE[0]),
        );
        gov.titles.insert(2, ("Hangar2".into(), corp_roles::ROLE_HANGAR_CAN_TAKE[1]));
        gov.titles.insert(3, ("BeanCounter".into(), corp_roles::ROLE_ACCOUNTANT));
        gov.titles.insert(4, ("Honorary".into(), 0));
        gov.titles.insert(5, (String::new(), 0));
    });

    // 1. L813: ExecuteActions releases unauthorized structure controller and restores ship when restored_ship > 0
    let stale_struct_id = 1_099_888_777_i64;
    let prev_station_id = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(stale_struct_id, ceo_id);
        ext.previous_ship_by_structure.insert(stale_struct_id, orig_ship_id);
        save_ext_state(snap, &ext);
        let (ch_sys, prev_station) = if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            let sys = ch.solar_system_id;
            let st = ch.station_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = stale_struct_id;
            (sys, st)
        } else {
            (30_000_142, None)
        };
        snap.structures.insert(
            stale_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: stale_struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: ch_sys,
                name: "Stale Citadel".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        prev_station
    });
    sess.ship_id = Some(stale_struct_id);
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut sess,
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(ceo_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("accountKey".into()),
                    PyRep::Integer(1001),
                ])]),
            ],
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(sess.ship_id, Some(orig_ship_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&stale_struct_id);
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = prev_station_id;
        }
    });
    let _ = ctx.drain_notifications_for_char(ceo_id);

    // 2. ExecuteActions role checks: non-director accountant denied at guard (CorpRoleDenied),
    //    personnel manager reaches legacy L894 check (CrpAccessDenied), non-CEO director allowed, CEO w/o dir bit allowed
    let mut non_ceo_dir_sess = sess.clone();
    non_ceo_dir_sess.character_id = Some(member_b);
    non_ceo_dir_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    let mut acct_only_sess = sess.clone();
    acct_only_sess.character_id = Some(member_id);
    acct_only_sess.corp_role = corp_roles::ROLE_ACCOUNTANT;
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut acct_only_sess,
            "ExecuteActions",
            &[PyRep::Integer(corp_id), PyRep::List(vec![PyRep::String("Ping".into())])],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let prev_mem_roles = ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        let prev = c.member_roles.get(&member_id).copied();
        c.member_roles.insert(member_id, corp_roles::ROLE_PERSONNEL_MANAGER);
        prev
    });
    let mut personnel_only_sess = sess.clone();
    personnel_only_sess.character_id = Some(member_id);
    personnel_only_sess.corp_role = corp_roles::ROLE_PERSONNEL_MANAGER;
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut personnel_only_sess,
            "ExecuteActions",
            &[PyRep::Integer(corp_id), PyRep::List(vec![PyRep::String("Ping".into())])],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        match prev_mem_roles {
            Some(r) => { c.member_roles.insert(member_id, r); }
            None => { c.member_roles.remove(&member_id); }
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(ceo_id, 0);
    });
    let mut ceo_no_dir_bit_sess = sess.clone();
    ceo_no_dir_bit_sess.corp_role = 0;
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut ceo_no_dir_bit_sess,
            "ExecuteActions",
            &[
                PyRep::Integer(corp_id),
                PyRep::List(vec![
                    PyRep::String("CeoRawAudit".into()),
                    PyRep::Tuple(vec![PyRep::String("SetFriendlyFire".into()), PyRep::Bool(true)]),
                ]),
            ],
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let gov = &snap.corp_governance[&corp_id];
        assert!(gov.action_log.iter().any(|(a, cid, _)| a == "CeoRawAudit" && *cid == ceo_id));
        assert_eq!(gov.pending_friendly_fire, Some((true, now + 86_400)));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(ceo_id, u64::MAX);
    });

    // 3. L916 & L1039 & L1006: Legacy SetAccountKey & SetNote exact change vs no-op notifications & self session update
    let _ = ctx.drain_notifications_for_char(ceo_id);
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut non_ceo_dir_sess,
            "ExecuteActions",
            &[
                PyRep::Integer(corp_id),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::String("SetAccountKey".into()),
                        PyRep::Integer(member_b),
                        PyRep::Integer(1002),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::String("SetAccountKey".into()),
                        PyRep::Integer(member_id),
                        PyRep::Integer(1005),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::String("SetNote".into()),
                        PyRep::Integer(member_id),
                        PyRep::String("Scout".into()),
                    ]),
                ]),
            ],
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(non_ceo_dir_sess.corp_account_key, 1002, "self SetAccountKey updates caller session");
    let corp_notes1 = ctx.drain_notifications_for_char(ceo_id);
    assert_eq!(
        corp_notes1.iter().filter(|n| n.event == "OnCorporationMemberChanged").count(),
        3
    );

    // Repeat identical SetAccountKey / SetNote -> 0 notifications!
    call_corp_reg(
        &ctx,
        &mut non_ceo_dir_sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::String("SetAccountKey".into()),
                    PyRep::Integer(member_b),
                    PyRep::Integer(1002),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("SetNote".into()),
                    PyRep::Integer(member_id),
                    PyRep::String("Scout".into()),
                ]),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(
        ctx.drain_notifications_for_char(ceo_id)
            .iter()
            .filter(|n| n.event == "OnCorporationMemberChanged")
            .count(),
        0,
        "unchanged accountKey/note must emit no OnCorporationMemberChanged"
    );

    // 4. L937..L989 & L1054..L1073: Legacy SetTitle edge cases (expired vs active pending, director xor, direct role overlap, exact r_at, notices)
    // Case A: non-hangar title 3 (ROLE_ACCOUNTANT) + non-role title 4 -> NO pending_title_role_grants, YES "roles" field when role changes, NO "roles" field when only title 4 toggles
    call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_id),
                PyRep::Integer(0b0100), // title 3 only (ROLE_ACCOUNTANT)
            ])]),
        ],
    )
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let gov = &snap.corp_governance[&corp_id];
        assert!(!gov.pending_title_role_grants.contains_key(&member_id));
        assert!(!gov.role_drop_timestamps.contains_key(&member_id));
    });
    let notes_role = ctx.drain_notifications_for_char(ceo_id);
    assert!(notes_role.iter().any(|n| {
        n.event == "OnCorporationMemberChanged"
            && n.args.get(2).is_some_and(|d| d.dict_get("roles").is_some())
    }));
    let has_sync = crate::services::fleet_social::fleet_social_persistence::with_social_state(&ctx, |s| {
        s.forced_session_corp_sync.contains_key(&member_id)
    });
    assert!(has_sync, "other member role change must populate forced_session_corp_sync");

    // Same mask no-op -> 0 notices; add title 4 (0 roles) -> notice WITHOUT "roles" field
    call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_id),
                PyRep::Integer(0b0100),
            ])]),
        ],
    )
    .unwrap();
    assert_eq!(
        ctx.drain_notifications_for_char(ceo_id)
            .iter()
            .filter(|n| n.event == "OnCorporationMemberChanged")
            .count(),
        0
    );
    call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_id),
                PyRep::Integer(0b1100), // add title 4 (0 roles)
            ])]),
        ],
    )
    .unwrap();
    let notes_no_role = ctx.drain_notifications_for_char(ceo_id);
    assert!(notes_no_role.iter().any(|n| {
        n.event == "OnCorporationMemberChanged"
            && n.args.get(2).is_some_and(|d| d.dict_get("titleMask").is_some() && d.dict_get("roles").is_none())
    }));

    // Case B: Non-CEO director keeping existing ROLE_DIRECTOR title 1 while adding title 2 succeeds (kills ^ -> |/& at L953),
    // while removing title 1 by non-CEO fails with CrpAccessDenied; also member_roles = ROLE_ACCOUNTANT (non-hangar) does NOT block hangar pending!
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_ACCOUNTANT);
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(member_id, 0b0001); // title 1 (has ROLE_DIRECTOR + HANGAR_TAKE_1)
        // Expired pending entry for a distinct hangar bit (e == now) -> promoted cleanly before ExecuteActions without satisfying title 2 (HANGAR_TAKE_2)
        gov.pending_title_role_grants
            .insert(member_id, (corp_roles::ROLE_HANGAR_CAN_TAKE[2] | corp_roles::ROLE_ACCOUNTANT, now));
    });
    assert!(call_corp_reg(
        &ctx,
        &mut non_ceo_dir_sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_id),
                PyRep::Integer(0b0011), // keep title 1 (ROLE_DIRECTOR), add title 2 (HANGAR_TAKE_2)
            ])]),
        ],
    )
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        let gov = &snap.corp_governance[&corp_id];
        assert_eq!(
            gov.pending_title_role_grants.get(&member_id).copied(),
            Some((corp_roles::ROLE_HANGAR_CAN_TAKE[1], now + 86_400)),
            "expired prev pending must not block fresh 24h pending_title_role_grants for HANGAR_TAKE_2"
        );
    });

    // Preserving active pending title while adding zero-role title 4 preserves prior ready_at != now + 86_400 (L969-972)
    let custom_ready = now + 12_345;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_title_role_grants
            .insert(member_id, (corp_roles::ROLE_HANGAR_CAN_TAKE[1] | corp_roles::ROLE_ACCOUNTANT, custom_ready));
    });
    assert!(call_corp_reg(
        &ctx,
        &mut non_ceo_dir_sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_id),
                PyRep::Integer(0b1011), // keep title 1 + 2, add title 4
            ])]),
        ],
    )
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corp_governance[&corp_id]
                .pending_title_role_grants
                .get(&member_id)
                .copied(),
            Some((corp_roles::ROLE_HANGAR_CAN_TAKE[1], custom_ready))
        );
    });

    // Self SetTitle updates session.corp_role (L1054) & non-CEO removing ROLE_DIRECTOR title fails (L953)
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut non_ceo_dir_sess,
            "ExecuteActions",
            &[
                PyRep::Integer(corp_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("SetTitle".into()),
                    PyRep::Integer(member_id),
                    PyRep::Integer(0b0010), // removes title 1 (ROLE_DIRECTOR)
                ])]),
            ],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(member_b, 0);
    });
    non_ceo_dir_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    call_corp_reg(
        &ctx,
        &mut non_ceo_dir_sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(member_b),
                PyRep::Integer(0b0100), // self grants title 3 (ROLE_ACCOUNTANT)
            ])]),
        ],
    )
    .unwrap();
    assert_ne!(
        non_ceo_dir_sess.corp_role & corp_roles::ROLE_ACCOUNTANT,
        0,
        "self SetTitle must update session.corp_role"
    );

    // 5. L1154-1155: handle_get_corp_market_info permission matrix
    // Member with disallowed role (ROLE_HANGAR_TAKE_1) denied; ROLE_TRADER allowed; non-member denied; CEO with 0 member_roles allowed
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.member_roles.insert(member_id, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
        c.member_roles.insert(ceo_id, 0);
        snap.corp_role_assignments.remove(&(corp_id, member_id));
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.remove(&member_id);
        gov.pending_role_grants.remove(&member_id);
        gov.pending_title_role_grants.remove(&member_id);
    });
    let mut hangar_sess = sess.clone();
    hangar_sess.character_id = Some(member_id);
    hangar_sess.corp_role = corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    assert_eq!(
        handle_get_corp_market_info(&ctx, &mut hangar_sess, &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_TRADER);
    });
    hangar_sess.corp_role = corp_roles::ROLE_TRADER;
    assert!(handle_get_corp_market_info(&ctx, &mut hangar_sess, &[]).unwrap().is_ok());

    let mut ceo_zero_sess = sess.clone();
    ceo_zero_sess.corp_role = 0;
    assert!(handle_get_corp_market_info(&ctx, &mut ceo_zero_sess, &[]).unwrap().is_ok());

    let mut outsider_sess = sess.clone();
    outsider_sess.character_id = Some(90_999_111);
    outsider_sess.corp_role = corp_roles::ROLE_TRADER;
    assert_eq!(
        handle_get_corp_market_info(&ctx, &mut outsider_sess, &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // 6. L1315: handle_get_member title filtering (non-empty active vs empty active title 5 vs inactive title 3)
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(member_id, (1u64 << 1) | (1u64 << 4)); // title 2 ("Hangar2") + title 5 ("")
    });
    let mem_kv = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(member_id)])
        .unwrap()
        .unwrap();
    assert_eq!(
        mem_kv.dict_get("title"),
        Some(&PyRep::String("Hangar2".into())),
        "empty title names and inactive title bits must both be excluded"
    );
}
