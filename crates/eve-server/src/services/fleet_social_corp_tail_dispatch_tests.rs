use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{CorpApplicationRecord, CorporationRecord, InventoryFlag, InventoryItemRecord};

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn corp_gov_tail_settings_assets_and_applications() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let corp_id = 98_000_010;
    let station_id = *ctx.sde.stations.keys().next().unwrap_or(&60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "tail corp".into();
            __corp.ticker = "TAIL".into();
            __corp.ceo_id = char_id;
            __corp.hq_station_id = Some(station_id);
            __corp.tax_rate = 0.1;
            __corp };
        corp.applications.insert(
            701,
            CorpApplicationRecord {
                application_id: 701,
                corporation_id: corp_id,
                character_id: char_id,
                message: "apply".into(),
                status: "Applied".into(),
                created_unix: 1000,
            },
        );
        snap.corporations.insert(corp_id, corp);
        snap.items.insert(
            920_001,
            InventoryItemRecord {
                item_id: 920_001,
                type_id: 27,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            920_002,
            InventoryItemRecord {
                item_id: 920_002,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = make_session(char_id, corp_id);
    let svc_reg = "corpRegistry";
    let svc_mgr = "corpmgr";
    let svc_st = "corpStationMgr";

    let kicks = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "GetPendingAutoKicks", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert!(!matches!(kicks, PyRep::None));

    let wm_none = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "GetCorpWelcomeMail", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(wm_none, PyRep::None);

    let set_wm = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "SetCorpWelcomeMail",
        &[PyRep::String("welcome pilot".into())],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_wm.dict_get("enabled").and_then(|v| v.as_bool()), Some(true));

    let get_wm = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "GetCorpWelcomeMail", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(get_wm.dict_get("enabled").and_then(|v| v.as_bool()), Some(true));

    let set_mail = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "RegisterNewCorpMailRestrictionSettings",
        &[PyRep::Bool(true)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_mail, PyRep::Bool(true));
    let get_mail = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "DoesCorpRestrictCorpMails", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(get_mail, PyRep::Bool(true));

    let set_acc = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(false)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_acc, PyRep::Bool(false));
    let get_acc = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "DoesMyCorpAcceptStructures", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(get_acc, PyRep::Bool(false));

    let set_reinf = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "SetStructureReinforceDefault",
        &[PyRep::Integer(20), PyRep::Integer(3)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_reinf, PyRep::Integer(20));
    let get_reinf = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "GetStructureReinforceDefault", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(get_reinf, PyRep::Tuple(vec![PyRep::Integer(20), PyRep::Integer(0)]));

    let divs = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "GetDivisionNames", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert!(matches!(divs, PyRep::KeyVal(_)));
    let deny_divs = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "GetDivisionNames",
        &[PyRep::Integer(corp_id + 99)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(deny_divs.code, "CrpAccessDenied");

    let attrs = handle_corp_gov_tail(&ctx, &mut session, svc_reg, "UpdateAttributes", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(attrs.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));

    let move_hq = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_st,
        "MoveCorpHQHere",
        &[PyRep::Integer(station_id)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(move_hq.dict_get("stationID").and_then(|v| v.as_i64()), Some(station_id));

    let assets = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_mgr,
        "GetAssetInventory",
        &[PyRep::Integer(corp_id), PyRep::String("offices".into())],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(assets, PyRep::None));

    let agg = handle_corp_gov_tail(
        &ctx,
        &mut session,
        svc_reg,
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
        corp_id,
        char_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        &agg,
        PyRep::Object { type_name, .. }
            if type_name == "crimewatch.corp_aggression.settings.AggressionSettings"
    ));
    assert_eq!(agg.dict_get("isFriendlyFireLegal").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        agg.dict_get("_enableAfter").and_then(|v| v.as_i64()),
        Some(crate::sp6_lifecycle::unix_to_filetime(1000 + 86_400))
    );

    let get_agg = handle_corp_gov_tail(&ctx, &mut session, svc_mgr, "GetAggressionSettings", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert!(matches!(
        &get_agg,
        PyRep::Object { type_name, .. }
            if type_name == "crimewatch.corp_aggression.settings.AggressionSettings"
    ));
    assert_eq!(get_agg.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));

    let my_apps = handle_get_my_applications_rpc(&ctx, &session, false).unwrap().unwrap();
    let PyRep::Dict(my_entries) = &my_apps else { panic!("expected Dict from GetMyApplications (#943)") };
    assert_eq!(my_entries.len(), 1);
    assert_eq!(my_entries[0].0, PyRep::Integer(corp_id));
    assert_eq!(my_entries[0].1.dict_get("applicationText"), Some(&PyRep::String("apply".into())));
    assert_eq!(
        my_entries[0].1.dict_get("applicationDateTime"),
        Some(&PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(1000)))
    );

    let corp_apps = handle_get_corp_applications_rpc(&ctx, &session, &[PyRep::Integer(corp_id)], false)
        .unwrap()
        .unwrap();
    let PyRep::Dict(corp_entries) = &corp_apps else { panic!("expected Dict from GetApplications (#943)") };
    assert_eq!(corp_entries.len(), 1);
    assert_eq!(corp_entries[0].0, PyRep::Integer(char_id));
    assert_eq!(corp_entries[0].1.dict_get("applicationText"), Some(&PyRep::String("apply".into())));
}

#[test]
fn corp_gov_tail_rbac_and_npc_corp_guards() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_000_777;
    let ceo_id = 2_000_701;
    let dir_id = 2_000_702;
    let personnel_id = 2_000_703;
    let chat_id = 2_000_704;
    let plain_id = 2_000_705;
    let npc_corp_id = 1_000_009;
    let missing_corp_id = 98_300_999;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "RbacCorp".into();
        corp.ticker = "RBAC".into();
        corp.ceo_id = ceo_id;
        corp.members.extend([ceo_id, dir_id, personnel_id, chat_id, plain_id]);
        corp.member_roles.insert(dir_id, eve_db::corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(personnel_id, eve_db::corp_roles::ROLE_PERSONNEL_MANAGER);
        corp.member_roles
            .insert(chat_id, eve_db::corp_roles::ROLE_CHAT_MANAGER);
        snap.corporations.insert(corp_id, corp);
    });

    let mut ceo_sess = make_session(ceo_id, corp_id);
    let mut dir_sess = make_session(dir_id, corp_id);
    let mut personnel_sess = make_session(personnel_id, corp_id);
    let mut chat_sess = make_session(chat_id, corp_id);
    let mut plain_sess = make_session(plain_id, corp_id);
    let mut npc_sess = make_session(plain_id, npc_corp_id);
    let mut missing_sess = make_session(plain_id, missing_corp_id);

    // GetPendingAutoKicks: Director & Personnel Manager allowed; Chat Manager, roleless, and NPC corp denied.
    assert!(handle_corp_gov_tail(
        &ctx,
        &mut dir_sess,
        "corpRegistry",
        "GetPendingAutoKicks",
        &[],
        corp_id,
        dir_id,
        1000,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corp_gov_tail(
        &ctx,
        &mut personnel_sess,
        "corpRegistry",
        "GetPendingAutoKicks",
        &[],
        corp_id,
        personnel_id,
        1000,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut chat_sess,
            "corpRegistry",
            "GetPendingAutoKicks",
            &[],
            corp_id,
            chat_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut plain_sess,
            "corpRegistry",
            "GetPendingAutoKicks",
            &[],
            corp_id,
            plain_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut npc_sess,
            "corpRegistry",
            "GetPendingAutoKicks",
            &[],
            npc_corp_id,
            plain_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // GetCorpWelcomeMail: Personnel Manager & Chat Manager allowed; roleless and NPC corp denied.
    assert!(handle_corp_gov_tail(
        &ctx,
        &mut personnel_sess,
        "corpRegistry",
        "GetCorpWelcomeMail",
        &[],
        corp_id,
        personnel_id,
        1000,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corp_gov_tail(
        &ctx,
        &mut chat_sess,
        "corpRegistry",
        "GetCorpWelcomeMail",
        &[],
        corp_id,
        chat_id,
        1000,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut plain_sess,
            "corpRegistry",
            "GetCorpWelcomeMail",
            &[],
            corp_id,
            plain_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut npc_sess,
            "corpRegistry",
            "GetCorpWelcomeMail",
            &[],
            npc_corp_id,
            plain_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // UpdateAttributes & UpdateCorporationAbilities: CEO/Director allowed; roleless, NPC corp, missing corp denied without mutating state.
    let denied_role_code = "CorpRoleDenied";
    let corp_missing_code = "CorporationNotFound";
    for method in ["UpdateAttributes", "UpdateCorporationAbilities"] {
        assert!(handle_corp_gov_tail(&ctx, &mut ceo_sess, "corpRegistry", method, &[], corp_id, ceo_id, 1000)
            .unwrap()
            .is_ok());
        assert_eq!(
            handle_corp_gov_tail(&ctx, &mut plain_sess, "corpRegistry", method, &[], corp_id, plain_id, 1000)
                .unwrap()
                .unwrap_err()
                .code,
            denied_role_code
        );
        assert_eq!(
            handle_corp_gov_tail(
                &ctx,
                &mut npc_sess,
                "corpRegistry",
                method,
                &[],
                npc_corp_id,
                plain_id,
                1000,
            )
            .unwrap()
            .unwrap_err()
            .code,
            denied_role_code
        );
        assert_eq!(
            handle_corp_gov_tail(
                &ctx,
                &mut missing_sess,
                "corpRegistry",
                method,
                &[],
                missing_corp_id,
                plain_id,
                1000,
            )
            .unwrap()
            .unwrap_err()
            .code,
            corp_missing_code
        );
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.corp_member_limits.contains_key(&npc_corp_id));
        assert!(!snap.corp_governance.contains_key(&npc_corp_id));
        assert!(!snap.corp_member_limits.contains_key(&missing_corp_id));
    });
}

#[test]
fn corp_gov_tail_enforces_non_empty_corp_members() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_600_701_i64;
    let corp_no_ceo = 98_600_702_i64;
    let ceo_id = 91_600_701_i64;
    let member_id = 91_600_702_i64;
    let omitted_id = 91_600_703_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "TailMemberCorp".into();
        corp.ticker = "TMC".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id]);
        corp.member_roles.insert(omitted_id, u64::MAX);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        let mut no_ceo = CorporationRecord::default();
        no_ceo.corporation_id = corp_no_ceo;
        no_ceo.name = "TailNoCeoCorp".into();
        no_ceo.ticker = "TNCC".into();
        no_ceo.ceo_id = 0;
        no_ceo.members = std::collections::HashSet::from([member_id]);
        no_ceo.member_roles.insert(omitted_id, u64::MAX);
        no_ceo.ensure_wallet_divisions();
        snap.corporations.insert(corp_no_ceo, no_ceo);
    });

    let mut omitted_sess = make_session(omitted_id, corp_id);
    for (svc, method, args) in [
        ("corpRegistry", "GetPendingAutoKicks", vec![]),
        ("corpRegistry", "UpdateAttributes", vec![]),
        ("corpRegistry", "UpdateCorporationAbilities", vec![]),
        ("corpStationMgr", "MoveCorpHQHere", vec![PyRep::Integer(60_003_760)]),
        (
            "corpmgr",
            "GetAssetInventory",
            vec![PyRep::Integer(corp_id), PyRep::String("offices".into())],
        ),
        ("corpRegistry", "RegisterNewAggressionSettings", vec![PyRep::Bool(true)]),
    ] {
        assert_eq!(
            handle_corp_gov_tail(&ctx, &mut omitted_sess, svc, method, &args, corp_id, omitted_id, 1000)
                .unwrap()
                .unwrap_err()
                .code,
            "CorpRoleDenied",
            "{svc}.{method} must deny character omitted from non-empty corp.members"
        );
    }

    // Even when ceo_id == 0, non-members omitted from non-empty corp.members are denied UpdateAttributes.
    let mut omitted_no_ceo_sess = make_session(omitted_id, corp_no_ceo);
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut omitted_no_ceo_sess,
            "corpRegistry",
            "UpdateAttributes",
            &[],
            corp_no_ceo,
            omitted_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // GetApplications also rejects callers omitted from non-empty corp.members.
    assert_eq!(
        handle_get_corp_applications_rpc(&ctx, &omitted_sess, &[PyRep::Integer(corp_id)], false)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
}

#[test]
fn corp_gov_tail_update_attributes_rejects_stale_session_corp_role() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_600_801_i64;
    let ceo_id = 91_600_801_i64;
    let plain_id = 91_600_802_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "AttrRoleCorp".into();
        corp.ticker = "ARC".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id, plain_id]);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });

    let mut plain_sess = make_session(plain_id, corp_id);
    plain_sess.corp_role = u64::MAX;
    assert_eq!(
        handle_corp_gov_tail(
            &ctx,
            &mut plain_sess,
            "corpRegistry",
            "UpdateAttributes",
            &[],
            corp_id,
            plain_id,
            1000,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut ceo_sess = make_session(ceo_id, corp_id);
    ceo_sess.corp_role = 0;
    assert!(
        handle_corp_gov_tail(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateAttributes",
            &[],
            corp_id,
            ceo_id,
            1000,
        )
        .unwrap()
        .is_ok()
    );
}
