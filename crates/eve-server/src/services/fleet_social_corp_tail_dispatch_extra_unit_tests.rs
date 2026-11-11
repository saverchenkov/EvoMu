//! Deep branch-coverage and mutation-killing unit tests for `fleet_social_corp_tail_dispatch`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_corp_tail_dispatch::{
    handle_corp_gov_tail, handle_get_corp_applications_rpc, handle_get_my_applications_rpc,
};
use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{
    corp_roles, CorpApplicationRecord, CorpWalletDivisionRecord, CorporationRecord, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub(super) fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: role,
        ..Default::default()
    }
}

pub(super) fn call_reg(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_corp_gov_tail(ctx, session, "corpRegistry", method, args, corp_id, char_id, now_unix)
}

pub(super) fn call_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_corp_gov_tail(ctx, session, "corpmgr", method, args, corp_id, char_id, now_unix)
}

pub(super) fn call_st(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_corp_gov_tail(ctx, session, "corpStationMgr", method, args, corp_id, char_id, now_unix)
}

#[test]
fn welcome_mail_restrictions_structures_reinforce_divisions_ally_cost_and_hq_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let ceo_id = 100_100_001i64;
    let comms_id = 100_100_002i64;
    let plain_id = 100_100_003i64;
    let corp_id = 98_300_001i64;
    let station_id = *ctx.sde.stations.keys().next().unwrap_or(&60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for cid in [ceo_id, comms_id, plain_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }

        let mut roles = HashMap::new();
        roles.insert(
            comms_id,
            corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER | (1u64 << 55),
        );
        roles.insert(plain_id, 0);

        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "GovernanceMatrixCorp".into();
            __corp.ticker = "GMXC".into();
            __corp.ceo_id = ceo_id;
            __corp.hq_station_id = None;
            __corp.members = HashSet::from([ceo_id, comms_id, plain_id]);
            __corp.member_roles = roles;
            __corp.wallet_divisions = vec![CorpWalletDivisionRecord {
                division_id: 1,
                name: "CustomMainWallet".into(),
                balance: IskCents(0),
            }];
            __corp };
        corp.applications.insert(
            1,
            CorpApplicationRecord {
                application_id: 1,
                corporation_id: corp_id,
                character_id: plain_id,
                message: "OpenApp".into(),
                status: "Invited".into(),
                created_unix: 1000,
            },
        );
        corp.applications.insert(
            2,
            CorpApplicationRecord {
                application_id: 2,
                corporation_id: corp_id,
                character_id: plain_id,
                message: "ClosedApp".into(),
                status: "Rejected".into(),
                created_unix: 900,
            },
        );
        snap.corporations.insert(corp_id, corp);

        let mut h_divs = HashMap::new();
        h_divs.insert(1u8, "CustomHangarOne".to_string());
        snap.corp_hangar_divisions.insert(corp_id, h_divs);

        snap.skills.entry(ceo_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 3,
                skill_points: 20_000.0,
            },
        );
    });

    with_corp_gov(&ctx, |g| {
        g.entry(corp_id)
            .or_default()
            .pending_auto_kicks
            .insert(plain_id, 1_700_000_500);
    });

    let mut plain_sess = make_session(plain_id, corp_id, 0);
    let mut comms_sess = make_session(comms_id, corp_id, 1u64 << 55);
    let mut ceo_sess = make_session(ceo_id, corp_id, u64::MAX);

    let other_method = "GetTime";
    assert!(call_reg(&ctx, &mut ceo_sess, other_method, &[], corp_id, ceo_id, 1000).is_none());

    // 1. Role denials for plain member on comms and director endpoints
    for m in [
        "SetCorpWelcomeMail",
        "RegisterNewCorpMailRestrictionSettings",
        "RegisterNewAcceptStructureSettings",
        "SetStructureReinforceDefault",
        "UpdateAttributes",
    ] {
        let e = call_reg(&ctx, &mut plain_sess, m, &[], corp_id, plain_id, 1000)
            .unwrap()
            .unwrap_err();
        assert_eq!(e.code, "CorpRoleDenied");
    }
    let e_hq = call_st(&ctx, &mut plain_sess, "MoveCorpHQHere", &[], corp_id, plain_id, 1000)
        .unwrap()
        .unwrap_err();
    assert_eq!(e_hq.code, "CorpRoleDenied");

    // 2. Communications Officer can set welcome mail via Dict/KeyVal, multi-arg, whitespace String, and None
    let kv_payload = PyRep::keyval([
        ("subject", PyRep::String("customsubj".into())),
        ("welcomeMail", PyRep::String("custombody".into())),
    ]);
    let r_kv = call_reg(
        &ctx,
        &mut comms_sess,
        "SetCorpWelcomeMail",
        &[kv_payload],
        corp_id,
        comms_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r_kv.dict_get("enabled").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(r_kv.dict_get("subject").and_then(|v| v.as_str()), Some("customsubj"));

    let r_multi = call_reg(
        &ctx,
        &mut comms_sess,
        "SetCorpWelcomeMail",
        &[
            PyRep::String("subj2".into()),
            PyRep::String("body2".into()),
            PyRep::Integer(0),
        ],
        corp_id,
        comms_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r_multi.dict_get("enabled").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        call_reg(&ctx, &mut comms_sess, "GetCorpWelcomeMail", &[], corp_id, comms_id, 1000)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    for arg in [PyRep::String("   ".into()), PyRep::None] {
        let r = call_reg(
            &ctx,
            &mut comms_sess,
            "SetCorpWelcomeMail",
            &[arg],
            corp_id,
            comms_id,
            1000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.dict_get("enabled").and_then(|v| v.as_bool()), Some(false));
    }

    // 3. SetStructureReinforceDefault clamps hour (0..=23) and weekday (0..=6); GetStructureReinforceDefault("weekday")
    let r_hr = call_reg(
        &ctx,
        &mut ceo_sess,
        "SetStructureReinforceDefault",
        &[PyRep::Integer(99), PyRep::Integer(4)],
        corp_id,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r_hr, PyRep::Integer(23));
    let r_wd = call_reg(
        &ctx,
        &mut ceo_sess,
        "GetStructureReinforceDefault",
        &[PyRep::String("weekday".into())],
        corp_id,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r_wd, PyRep::Integer(4));

    // 4. GetDivisionNames returns custom hangar 1 and custom wallet 1
    let divs = call_mgr(
        &ctx,
        &mut plain_sess,
        "GetDivisionNames",
        &[PyRep::Integer(corp_id)],
        corp_id,
        plain_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        divs.dict_get("division1").and_then(|v| v.as_str()),
        Some("CustomHangarOne")
    );
    assert_eq!(
        divs.dict_get("walletDivision1").and_then(|v| v.as_str()),
        Some("CustomMainWallet")
    );

    // 5. CharGetAllyBaseCost and UpdateCorporationAbilities
    assert!(call_reg(&ctx, &mut ceo_sess, "CharGetAllyBaseCost", &[], corp_id, ceo_id, 1000)
        .unwrap()
        .is_ok());
    let abilities = call_reg(
        &ctx,
        &mut ceo_sess,
        "UpdateCorporationAbilities",
        &[],
        corp_id,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(abilities.dict_get("memberLimit").and_then(|v| v.as_i64()).unwrap_or(0) >= 10);

    // 6. MoveCorpHQHere: missing corp -> CorporationNotFound; no office -> NoOfficeAtStation
    let e_nocorp = call_st(
        &ctx,
        &mut ceo_sess,
        "MoveCorpHQHere",
        &[PyRep::Integer(station_id)],
        999_999_999,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_nocorp.code, "CorporationNotFound");

    let e_nooff = call_st(
        &ctx,
        &mut ceo_sess,
        "MoveCorpHQHere",
        &[PyRep::Integer(station_id)],
        corp_id,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_nooff.code, "NoOfficeAtStation");

    // 7. Applications RPCs: open (is_old=false) vs closed (is_old=true) and role guards
    let unauth = SessionState::default();
    assert!(handle_get_my_applications_rpc(&ctx, &unauth, false).unwrap().is_err());
    assert!(handle_get_corp_applications_rpc(&ctx, &unauth, &[], false).unwrap().is_err());

    let e_app_nocorp =
        handle_get_corp_applications_rpc(&ctx, &ceo_sess, &[PyRep::Integer(999_888_777)], false)
            .unwrap()
            .unwrap_err();
    assert_eq!(e_app_nocorp.code, "CorporationNotFound");

    let e_app_role =
        handle_get_corp_applications_rpc(&ctx, &plain_sess, &[PyRep::Integer(corp_id)], false)
            .unwrap()
            .unwrap_err();
    assert_eq!(e_app_role.code, "CorpRoleDenied");

    assert!(!matches!(
        handle_get_my_applications_rpc(&ctx, &plain_sess, false).unwrap().unwrap(),
        PyRep::None
    ));
    assert!(!matches!(
        handle_get_my_applications_rpc(&ctx, &plain_sess, true).unwrap().unwrap(),
        PyRep::None
    ));
    assert!(!matches!(
        handle_get_corp_applications_rpc(&ctx, &ceo_sess, &[PyRep::Integer(corp_id)], true)
            .unwrap()
            .unwrap(),
        PyRep::None
    ));
}

#[test]
fn mutants_cov_corp_gov_tail_assets_aggression_and_hq_edges() {
    let now_unix = 1_700_000_000_u64;
    let (ctx, _clock) = test_ctx_and_clock(now_unix);
    let ceo_id = 100_200_001_i64;
    let dir_id = 100_200_002_i64;
    let acc_id = 100_200_003_i64;
    let jacc_id = 100_200_004_i64;
    let trader_id = 100_200_005_i64;
    let chat_id = 100_200_006_i64;
    let plain_id = 100_200_007_i64;
    let corp_id = 98_300_101_i64;
    let corp_no_ceo = 98_300_102_i64;

    let mut stations = ctx.sde.stations.keys().copied().collect::<Vec<_>>();
    stations.sort_unstable();
    let st1 = stations[0];
    let st2 = *stations.get(1).unwrap_or(&st1);
    let office1_id = 1_000_770_001_i64;
    let office2_id = 1_000_770_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for cid in [ceo_id, dir_id, acc_id, jacc_id, trader_id, chat_id, plain_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }
        let mut roles = HashMap::new();
        roles.insert(dir_id, corp_roles::ROLE_DIRECTOR);
        roles.insert(acc_id, corp_roles::ROLE_ACCOUNTANT);
        roles.insert(jacc_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
        roles.insert(trader_id, corp_roles::ROLE_TRADER);
        roles.insert(chat_id, corp_roles::ROLE_CHAT_MANAGER);
        roles.insert(plain_id, 0);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "MutantTailCorp".into();
        corp.ticker = "MTC".into();
        corp.ceo_id = ceo_id;
        corp.members = HashSet::from([ceo_id, dir_id, acc_id, jacc_id, trader_id, chat_id, plain_id]);
        corp.member_roles = roles;
        snap.corporations.insert(corp_id, corp);

        let mut npc_like = CorporationRecord::default();
        npc_like.corporation_id = corp_no_ceo;
        npc_like.name = "ZeroCeoCorp".into();
        npc_like.ticker = "ZCC".into();
        npc_like.ceo_id = 0;
        npc_like.members = HashSet::from([plain_id]);
        snap.corporations.insert(corp_no_ceo, npc_like);

        // Corp offices + assets across hangar, deliveries, lockdown, impounded
        let mk = |iid: i64, tid: u32, loc: i64, flag: eve_db::InventoryFlag, qty: i32| {
            eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: tid,
                owner_id: corp_id,
                location_id: loc,
                flag,
                quantity: qty,
                singleton: false,
                ..Default::default()
            }
        };
        snap.items.insert(office1_id, mk(office1_id, 27, st1, eve_db::InventoryFlag::Hangar, 1));
        snap.items.insert(office2_id, mk(office2_id, 27, st2, eve_db::InventoryFlag::Hangar, 1));
        snap.items.insert(1_000_770_010, mk(1_000_770_010, 34, office1_id, eve_db::InventoryFlag::CorpSAG1, 50));
        snap.items.insert(1_000_770_011, mk(1_000_770_011, 34, st1, eve_db::InventoryFlag::Deliveries, 25));
        snap.items.insert(1_000_770_012, mk(1_000_770_012, 34, st1, eve_db::InventoryFlag::Locked, 10));
        snap.items.insert(1_000_770_013, mk(1_000_770_013, 34, st2, eve_db::InventoryFlag::Impounded, 15));
    });

    // 1. UpdateAttributes:
    // - CEO without ROLE_DIRECTOR in session/member_roles still succeeds
    // - non-CEO Director succeeds
    // - plain member in corp with ceo_id != 0 fails CorpRoleDenied
    // - plain member in corp with ceo_id == 0 succeeds (kills `corp.ceo_id != 0` -> `==`)
    let mut ceo_no_dir_sess = make_session(ceo_id, corp_id, 0);
    let r_ceo = call_reg(&ctx, &mut ceo_no_dir_sess, "UpdateAttributes", &[], corp_id, ceo_id, now_unix)
        .unwrap()
        .unwrap();
    assert_eq!(r_ceo.dict_get("corporationID"), Some(&PyRep::Integer(corp_id)));

    let mut dir_sess = make_session(dir_id, corp_id, corp_roles::ROLE_DIRECTOR);
    assert!(call_reg(&ctx, &mut dir_sess, "UpdateCorporationAbilities", &[], corp_id, dir_id, now_unix)
        .unwrap()
        .is_ok());

    let mut plain_sess = make_session(plain_id, corp_id, 0);
    assert_eq!(
        call_reg(&ctx, &mut plain_sess, "UpdateAttributes", &[], corp_id, plain_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert!(
        call_reg(&ctx, &mut plain_sess, "UpdateAttributes", &[], corp_no_ceo, plain_id, now_unix)
            .unwrap()
            .is_ok()
    );

    // 2. MoveCorpHQHere: non-CEO Director and CEO succeed with real office; plain_sess rejected
    assert_eq!(
        call_st(&ctx, &mut plain_sess, "MoveCorpHQHere", &[PyRep::Integer(st1)], corp_id, plain_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    let hq_dir = call_st(&ctx, &mut dir_sess, "MoveCorpHQHere", &[PyRep::Integer(st1)], corp_id, dir_id, now_unix)
        .unwrap()
        .unwrap();
    assert_eq!(hq_dir.dict_get("stationID"), Some(&PyRep::Integer(st1)));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].hq_station_id), Some(st1));

    let hq_ceo = call_st(&ctx, &mut ceo_no_dir_sess, "MoveCorpHQHere", &[PyRep::Integer(st2)], corp_id, ceo_id, now_unix)
        .unwrap()
        .unwrap();
    assert_eq!(hq_ceo.dict_get("stationID"), Some(&PyRep::Integer(st2)));

    // 3. GetAssetInventory / GetAssetInventoryForLocation / SearchAssets role & filter branches
    assert_eq!(
        call_mgr(&ctx, &mut dir_sess, "GetAssetInventory", &[PyRep::Integer(999_888_777)], corp_id, dir_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorporationNotFound"
    );
    let mut chat_sess = make_session(chat_id, corp_id, corp_roles::ROLE_CHAT_MANAGER);
    assert_eq!(
        call_mgr(&ctx, &mut chat_sess, "GetAssetInventory", &[PyRep::Integer(corp_id)], corp_id, chat_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_mgr(&ctx, &mut plain_sess, "GetAssetInventory", &[PyRep::Integer(corp_id)], corp_id, plain_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Every single allowed role (CEO, Director, Accountant, Junior Accountant, Trader) succeeds
    for (uid, role) in [
        (ceo_id, 0u64),
        (dir_id, corp_roles::ROLE_DIRECTOR),
        (acc_id, corp_roles::ROLE_ACCOUNTANT),
        (jacc_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
        (trader_id, corp_roles::ROLE_TRADER),
    ] {
        let mut s = make_session(uid, corp_id, role);
        let PyRep::Rowset { rows, .. } = call_mgr(
            &ctx,
            &mut s,
            "GetAssetInventory",
            &[PyRep::Integer(corp_id), PyRep::String("offices".into())],
            corp_id,
            uid,
            now_unix,
        )
        .unwrap()
        .unwrap()
        else {
            panic!("expected rowset");
        };
        assert_eq!(rows.len(), 1);
    }
    let PyRep::Rowset { rows: deliv_rows, .. } = call_mgr(
        &ctx,
        &mut dir_sess,
        "GetAssetInventoryForLocation",
        &[PyRep::Integer(corp_id), PyRep::Integer(st1), PyRep::String("deliveries".into())],
        corp_id,
        dir_id,
        now_unix,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected rowset");
    };
    assert_eq!(deliv_rows.len(), 1);
    let PyRep::Rowset { rows: lock_rows, .. } = call_mgr(
        &ctx,
        &mut dir_sess,
        "SearchAssets",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("lockdown".into()),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(34),
            PyRep::Integer(10),
        ],
        corp_id,
        dir_id,
        now_unix,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected rowset");
    };
    assert_eq!(lock_rows.len(), 1);

    // 4. AggressionSettings: GetAggressionSettings, GetAggressionSettingsForCorps, RegisterNewAggressionSettings
    assert_eq!(
        call_reg(&ctx, &mut dir_sess, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], 999_888_777, dir_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_reg(&ctx, &mut plain_sess, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], corp_id, plain_id, now_unix)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    let expected_ft = crate::sp6_lifecycle::unix_to_filetime(now_unix + 86_400);
    // Register true when currently false -> _enableAfter = expected_ft, _disableAfter = 0
    let reg_on = call_reg(&ctx, &mut dir_sess, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], corp_id, dir_id, now_unix)
        .unwrap()
        .unwrap();
    let PyRep::Object { args: reg_on_args, .. } = reg_on else { panic!("expected Object") };
    assert_eq!(reg_on_args.dict_get("_enableAfter"), Some(&PyRep::Integer(expected_ft)));
    assert_eq!(reg_on_args.dict_get("_disableAfter"), Some(&PyRep::Integer(0)));
    assert_eq!(reg_on_args.dict_get("changeAtTime"), Some(&PyRep::Integer(expected_ft)));

    // GetAggressionSettings before effective_at -> matches pending enableAfter
    let get_pending_on = call_mgr(&ctx, &mut plain_sess, "GetAggressionSettings", &[PyRep::Integer(corp_id)], corp_id, plain_id, now_unix + 100)
        .unwrap()
        .unwrap();
    let PyRep::Object { args: gp_on_args, .. } = get_pending_on else { panic!() };
    assert_eq!(gp_on_args.dict_get("_enableAfter"), Some(&PyRep::Integer(expected_ft)));
    assert_eq!(gp_on_args.dict_get("_disableAfter"), Some(&PyRep::Integer(0)));

    // Set current friendly_fire_legal = true and register false -> _enableAfter = 0, _disableAfter = expected_ft
    ctx.db.with_mut_snapshot(|s| {
        let g = s.corp_governance.entry(corp_id).or_default();
        g.friendly_fire_legal = true;
        g.pending_friendly_fire = None;
    });
    let reg_off = call_reg(&ctx, &mut ceo_no_dir_sess, "RegisterNewAggressionSettings", &[PyRep::Bool(false)], corp_id, ceo_id, now_unix)
        .unwrap()
        .unwrap();
    let PyRep::Object { args: reg_off_args, .. } = reg_off else { panic!() };
    assert_eq!(reg_off_args.dict_get("_enableAfter"), Some(&PyRep::Integer(0)));
    assert_eq!(reg_off_args.dict_get("_disableAfter"), Some(&PyRep::Integer(expected_ft)));

    let get_pending_off = call_reg(&ctx, &mut plain_sess, "GetAggressionSettings", &[PyRep::Integer(corp_id)], corp_id, plain_id, now_unix + 100)
        .unwrap()
        .unwrap();
    let PyRep::Object { args: gp_off_args, .. } = get_pending_off else { panic!() };
    assert_eq!(gp_off_args.dict_get("_enableAfter"), Some(&PyRep::Integer(0)));
    assert_eq!(gp_off_args.dict_get("_disableAfter"), Some(&PyRep::Integer(expected_ft)));

    let for_corps = call_mgr(
        &ctx,
        &mut plain_sess,
        "GetAggressionSettingsForCorps",
        &[PyRep::List(vec![PyRep::Integer(corp_id), PyRep::Integer(corp_no_ceo)])],
        corp_id,
        plain_id,
        now_unix,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(fc_entries) = for_corps else { panic!("expected Dict") };
    assert_eq!(fc_entries.len(), 2);
}
