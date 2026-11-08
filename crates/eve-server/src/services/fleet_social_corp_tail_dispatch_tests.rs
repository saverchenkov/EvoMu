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
    assert_eq!(get_reinf, PyRep::Integer(20));

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
    assert_eq!(agg.dict_get("isFriendlyFireLegal").and_then(|v| v.as_bool()), Some(true));

    let get_agg = handle_corp_gov_tail(&ctx, &mut session, svc_mgr, "GetAggressionSettings", &[], corp_id, char_id, 1000)
        .unwrap()
        .unwrap();
    assert_eq!(get_agg.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));

    let my_apps = handle_get_my_applications_rpc(&ctx, &session, false).unwrap().unwrap();
    assert!(!matches!(my_apps, PyRep::None));
    let corp_apps = handle_get_corp_applications_rpc(&ctx, &session, &[PyRep::Integer(corp_id)], false)
        .unwrap()
        .unwrap();
    assert!(!matches!(corp_apps, PyRep::None));
}
