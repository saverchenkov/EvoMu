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

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "GovernanceMatrixCorp".into(),
            ticker: "GMXC".into(),
            ceo_id,
            hq_station_id: None,
            members: HashSet::from([ceo_id, comms_id, plain_id]),
            member_roles: roles,
            wallet_divisions: vec![CorpWalletDivisionRecord {
                division_id: 1,
                name: "CustomMainWallet".into(),
                balance: IskCents(0),
            }],
            ..Default::default()
        };
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
