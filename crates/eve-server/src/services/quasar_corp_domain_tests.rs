//! Unit tests for `quasar_corp_domain` (role/flag parsing, corp authorization, alliance/corp Quasar gateway handlers).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::corp_roles;
use eve_db::{AllianceRecord, CorporationRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::default())))
}

fn seq_id_msg(field: u32, id: u32) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("SequentialId");
    inner.fields.insert(1, DynamicValue::Uint32(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn helper_parsers_role_masks_and_corp_authorization() {
    assert_eq!(parse_location_flag("Hangar"), 4);
    assert_eq!(parse_location_flag("Cargo"), 5);
    assert_eq!(parse_location_flag("CorpSAG1"), 116);
    assert_eq!(parse_location_flag("CorpSAG4"), 119);
    assert_eq!(parse_location_flag("CorpSAG7"), 122);
    assert_eq!(parse_location_flag("999"), 999);
    assert_eq!(parse_location_flag("unknown"), 4);

    assert_eq!(starbase_role_enum("config_starbase_equipment"), 1);
    assert_eq!(starbase_role_enum("corporation_member"), 2);
    assert_eq!(starbase_role_enum("alliance_member"), 3);
    assert_eq!(starbase_role_enum("starbase_fuel_technician"), 4);
    assert_eq!(starbase_role_enum("anything_else"), 2);

    let mask = role_names_to_mask(&[
        "Director".into(),
        "PersonnelManager".into(),
        "Accountant".into(),
        "SecurityOfficer".into(),
        "FactoryManager".into(),
        "StationManager".into(),
        "Auditor".into(),
        "Diplomat".into(),
        "Trader".into(),
    ]);
    assert_ne!(mask & corp_roles::ROLE_DIRECTOR, 0);
    assert_ne!(mask & corp_roles::ROLE_ACCOUNTANT, 0);
    assert_ne!(mask & corp_roles::ROLE_TRADER, 0);

    let role_buf = build_role_collection(mask).must_encode();
    assert!(!role_buf.is_empty());

    assert!(!build_isk_currency(-12_345).must_encode().is_empty());
    assert!(!build_standing_value(5.75).must_encode().is_empty());

    let db = WorldRepository::with_demo_seed();
    let cid = 980_001_i64;
    let corp_id = 98_800_001_i64;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.corporation_id = corp_id;
        snap.characters.insert(cid, ch);

        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = cid;
        snap.corporations.insert(corp_id, c);
        snap.structures.insert(
            88_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 88_000_001,
                type_id: 16_213,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        );
        snap.structures.insert(
            88_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 88_000_001,
                type_id: 16_213,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(88_000_001, Default::default());
    });

    // Unauthenticated context rejected
    let unauth = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    assert_eq!(
        resolve_authorized_corp(&unauth, &DynamicQuasarMessage::new("Req"), 1, &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Same-corp authorized, cross-corp denied for non-admin
    let ctx_player = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        resolve_authorized_corp(&ctx_player, &seq_id_msg(1, corp_id as u32), 1, &db),
        Ok(corp_id)
    );
    assert_eq!(
        resolve_authorized_corp(&ctx_player, &seq_id_msg(1, 98_999_999), 1, &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let (admin_cid, admin_corp) = check_corp_admin_role(&ctx_player, &db).expect("director passes admin check");
    assert_eq!(admin_cid, cid as u32);
    assert_eq!(admin_corp, corp_id);

    let ctx_grunt = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32 + 1),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        check_corp_admin_role(&ctx_grunt, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
}

#[test]
fn registered_quasar_alliance_and_corporation_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 980_010_i64;
    let corp_id = 98_800_010_i64;
    let ally_id = 99_800_010_i64;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(ally_id);
        snap.characters.insert(cid, ch);

        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = cid;
        c.hq_station_id = Some(60_003_760);
        c.alliance_id = Some(ally_id);
        c.name = "QuasarTestCorp".into();
        c.ticker = "QSTC".into();
        snap.corporations.insert(corp_id, c);

        let mut members = HashSet::new();
        members.insert(corp_id);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "QuasarTestAlliance".into(),
                short_name: "QSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: members,
                ..Default::default()
            },
        );
        snap.structures.insert(
            88_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 88_000_001,
                type_id: 16_213,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(88_000_001, Default::default());
    });

    let auth_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    // Alliance handlers
    let ally_get = handlers["eve.alliance.GetRequest"](
        &auth_ctx,
        &dummy_any,
        &seq_id_msg(1, ally_id as u32),
    )
    .expect("eve.alliance.GetRequest");
    assert!(ally_get.type_url.ends_with("eve.alliance.GetResponse"));
    assert!(!ally_get.value.is_empty());

    let ally_missing = handlers["eve.alliance.GetRequest"](
        &auth_ctx,
        &dummy_any,
        &seq_id_msg(1, 111_222_333),
    )
    .unwrap_err();
    assert_eq!(ally_missing.0, STATUS_NOT_FOUND);

    let ally_all = handlers["eve.alliance.GetAllRequest"](
        &auth_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("GetAll"),
    )
    .expect("eve.alliance.GetAllRequest");
    assert!(ally_all.type_url.ends_with("eve.alliance.GetAllResponse"));

    let mut search_msg = DynamicQuasarMessage::new("Search");
    search_msg
        .fields
        .insert(1, DynamicValue::String("QuasarTest".into()));
    let ally_search = handlers["eve.alliance.SearchRequest"](&auth_ctx, &dummy_any, &search_msg)
        .expect("eve.alliance.SearchRequest");
    assert!(ally_search.type_url.ends_with("eve.alliance.SearchResponse"));

    // Invoke all standard corp domain read handlers with corp seq_id
    let mut corp_msg = seq_id_msg(1, corp_id as u32);
    let mut sb_id_inner = DynamicQuasarMessage::new("SequentialIdentifier");
    sb_id_inner.fields.insert(1, DynamicValue::Uint32(88_000_001));
    corp_msg
        .fields
        .insert(3, DynamicValue::Message(Box::new(sb_id_inner)));
    for (req_name, expected_resp) in [
        (
            "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
            "eve.corporation.accounting.wallet.GetAllDivisionBalanceResponse",
        ),
        (
            "eve.corporation.divisions.GetRequest",
            "eve.corporation.divisions.GetResponse",
        ),
        (
            "eve.corporation.homestation.GetRequest",
            "eve.corporation.homestation.GetResponse",
        ),
        (
            "eve.corporation.alliance.api.GetCurrentMembershipRequest",
            "eve.corporation.alliance.api.GetCurrentMembershipResponse",
        ),
        (
            "eve.corporation.alliancemembershiprecord.GetAllRequest",
            "eve.corporation.alliancemembershiprecord.GetAllResponse",
        ),
        (
            "eve.corporation.alsclogs.GetRequest",
            "eve.corporation.alsclogs.GetResponse",
        ),
        (
            "eve.corporation.industry.job.GetActiveRequest",
            "eve.corporation.industry.job.GetActiveResponse",
        ),
        (
            "eve.corporation.industry.blueprints.GetRequest",
            "eve.corporation.industry.blueprints.GetResponse",
        ),
        (
            "eve.corporation.industry.facilties.GetRequest",
            "eve.corporation.industry.facilties.GetResponse",
        ),
        (
            "eve.corporation.industry.observer.GetAllDetailsRequest",
            "eve.corporation.industry.observer.GetAllDetailsResponse",
        ),
        (
            "eve.corporation.members.GetRequest",
            "eve.corporation.members.GetResponse",
        ),
        (
            "eve.corporation.members.GetLimitRequest",
            "eve.corporation.members.GetLimitResponse",
        ),
        (
            "eve.corporation.members.roles.GetRequest",
            "eve.corporation.members.roles.GetResponse",
        ),
        (
            "eve.corporation.members.titles.GetRequest",
            "eve.corporation.members.titles.GetResponse",
        ),
        (
            "eve.corporation.members.tracking.GetRequest",
            "eve.corporation.members.tracking.GetResponse",
        ),
        (
            "eve.corporation.members.roles.auditlog.GetRequest",
            "eve.corporation.members.roles.auditlog.GetResponse",
        ),
        (
            "eve.corporation.npc.GetAllRequest",
            "eve.corporation.npc.GetAllResponse",
        ),
        (
            "eve.corporation.office.api.GetSolarSystemsRequest",
            "eve.corporation.office.api.GetSolarSystemsResponse",
        ),
        (
            "eve.corporation.structure.GetAllWithDetailsRequest",
            "eve.corporation.structure.GetAllWithDetailsResponse",
        ),
        (
            "eve.corporation.title.GetAllRequest",
            "eve.corporation.title.GetAllResponse",
        ),
        (
            "eve.corporation.starbase.GetAllRequest",
            "eve.corporation.starbase.GetAllResponse",
        ),
        (
            "eve.corporation.starbase.GetSettingsAndFuelsRequest",
            "eve.corporation.starbase.GetSettingsAndFuelsResponse",
        ),
        (
            "eve.corporation.medals.GetRequest",
            "eve.corporation.medals.GetResponse",
        ),
        (
            "eve.corporation.medals.issued.GetRequest",
            "eve.corporation.medals.issued.GetResponse",
        ),
        (
            "eve.corporation.market.entry.GetEntriesRequest",
            "eve.corporation.market.entry.GetEntriesResponse",
        ),
        (
            "eve.corporation.standing.npc.GetAllRequest",
            "eve.corporation.standing.npc.GetAllResponse",
        ),
        (
            "eve.corporation.planetinteraction.office.GetRequest",
            "eve.corporation.planetinteraction.office.GetResponse",
        ),
    ] {
        let resp = handlers[req_name](&auth_ctx, &dummy_any, &corp_msg)
            .unwrap_or_else(|e| panic!("handler {req_name} failed: {e:?}"));
        assert!(
            resp.type_url.ends_with(expected_resp),
            "{req_name} produced unexpected type_url {}",
            resp.type_url
        );
    }
}

#[test]
fn corp_domain_builders_decode_strictly_with_descriptors() {
    let reg = Arc::new(
        QuasarDescriptorRegistry::open_default().expect("open default descriptor DB"),
    );
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(Arc::clone(&reg), Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 980_020_i64;
    let corp_id = 98_800_020_i64;
    let ally_id = 99_800_020_i64;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(ally_id);
        ch.station_id = Some(60_003_760);
        snap.characters.insert(cid, ch);

        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = cid;
        c.hq_station_id = Some(60_003_760);
        c.alliance_id = Some(ally_id);
        c.name = "StrictCorp".into();
        c.ticker = "STRC".into();
        c.members.insert(cid);
        snap.corporations.insert(corp_id, c);

        let mut ally_corps = HashSet::new();
        ally_corps.insert(corp_id);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "StrictAlliance".into(),
                short_name: "STRA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: ally_corps,
                ..Default::default()
            },
        );
        snap.corp_alliance_history.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpAllianceHistoryEntry {
                record_id: 1,
                alliance_id: Some(ally_id),
                start_date_unix: 1_700_000_000,
                is_deleted: Some(false),
            }],
        );
        snap.corp_container_logs.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpContainerLogEntry {
                logged_at_unix: 1_700_000_100,
                container_id: 1_000_000_001,
                container_type_id: 17366,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".into(),
                action: "configure".into(),
                character_id: cid,
                old_config_bitmask: Some(1),
                new_config_bitmask: Some(3),
                ..Default::default()
            }],
        );
        snap.industry_jobs.insert(
            5001,
            eve_db::IndustryJobRecord {
                job_id: 5001,
                installer_id: cid,
                facility_id: 60_003_760,
                activity_id: 1,
                blueprint_item_id: 9_001,
                blueprint_type_id: 687,
                product_type_id: 587,
                runs: 2,
                duration_seconds: 600,
                status: "Active".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            88_000_020,
            eve_db::UpwellStructureRecord {
                structure_id: 88_000_020,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Strict Fortizar".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.observer_mining_ledger.insert(88_000_020, Default::default());
        snap.starbase_settings.insert(88_000_020, Default::default());
        snap.corp_role_history.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpRoleHistoryEntry {
                changed_at_unix: 1_700_000_200,
                character_id: cid,
                issuer_id: cid,
                role_type: "roles".into(),
                old_roles: vec!["Accountant".into()],
                new_roles: vec!["Director".into()],
            }],
        );
        snap.corp_titles.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpTitleEntry {
                title_id: 1,
                name: "Officer".into(),
                roles: vec!["Director".into()],
                ..Default::default()
            }],
        );
        snap.corp_member_titles.insert((corp_id, cid), vec![1]);
        snap.corp_vote_cases.insert(
            902,
            eve_db::snapshot_ext::CorpVoteCaseRecord {
                case_id: 902,
                corporation_id: corp_id,
                ..Default::default()
            },
        );
        snap.corp_medals.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpMedalEntry {
                medal_id: 10,
                title: "Medal".into(),
                description: "Desc".into(),
                ..Default::default()
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CorpIssuedMedalEntry {
                medal_id: 10,
                character_id: cid,
                reason: "Valor".into(),
                ..Default::default()
            }],
        );
        snap.corp_wallet_transactions.insert(
            (corp_id, 1),
            vec![eve_db::snapshot_ext::WalletTransactionRecord {
                transaction_id: 77,
                date_unix: 1_700_000_300,
                unit_price: 12.5,
                quantity: 4,
                location_id: 60_003_760,
                client_id: cid,
                journal_ref_id: 88,
                type_id: 34,
                is_buy: true,
                is_personal: false,
            }],
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![eve_db::snapshot_ext::CustomsOfficeRecord {
                office_id: 77_000_001,
                system_id: 30_000_142,
                corporation_tax_rate: Some(0.05),
                allow_alliance_access: true,
                alliance_tax_rate: Some(0.02),
                allow_access_with_standings: true,
                ..Default::default()
            }],
        );
    });

    let auth_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    let mut corp_msg = seq_id_msg(1, corp_id as u32);
    let mut sb_id_inner = DynamicQuasarMessage::new("SequentialIdentifier");
    sb_id_inner.fields.insert(1, DynamicValue::Uint64(88_000_020));
    corp_msg
        .fields
        .insert(3, DynamicValue::Message(Box::new(sb_id_inner)));

    let cases: Vec<(&str, DynamicQuasarMessage)> = vec![
        ("eve.alliance.GetRequest", seq_id_msg(1, ally_id as u32)),
        ("eve.alliance.GetAllRequest", DynamicQuasarMessage::new("Req")),
        ("eve.alliance.SearchRequest", DynamicQuasarMessage::new("Req")),
        ("eve.alliance.corporation.GetMembersRequest", seq_id_msg(1, ally_id as u32)),
        ("eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest", corp_msg.clone()),
        ("eve.corporation.divisions.GetRequest", corp_msg.clone()),
        ("eve.corporation.homestation.GetRequest", corp_msg.clone()),
        ("eve.corporation.alliance.api.GetCurrentMembershipRequest", corp_msg.clone()),
        ("eve.corporation.alliancemembershiprecord.GetAllRequest", corp_msg.clone()),
        ("eve.corporation.alsclogs.GetRequest", corp_msg.clone()),
        ("eve.corporation.industry.job.GetActiveRequest", corp_msg.clone()),
        ("eve.corporation.industry.blueprints.GetRequest", corp_msg.clone()),
        ("eve.corporation.industry.facilties.GetRequest", corp_msg.clone()),
        ("eve.corporation.industry.observer.GetAllDetailsRequest", corp_msg.clone()),
        ("eve.corporation.members.GetRequest", corp_msg.clone()),
        ("eve.corporation.members.GetLimitRequest", corp_msg.clone()),
        ("eve.corporation.members.roles.GetRequest", corp_msg.clone()),
        ("eve.corporation.members.titles.GetRequest", corp_msg.clone()),
        ("eve.corporation.members.tracking.GetRequest", corp_msg.clone()),
        ("eve.corporation.members.roles.auditlog.GetRequest", corp_msg.clone()),
        ("eve.corporation.npc.GetAllRequest", DynamicQuasarMessage::new("Req")),
        ("eve.corporation.npc.IsNPCCorporationRequest", seq_id_msg(1, 1_000_009)),
        ("eve.corporation.logo.api.GetRequest", corp_msg.clone()),
        ("eve.corporation.office.api.GetSolarSystemsRequest", corp_msg.clone()),
        ("eve.corporation.structure.GetAllWithDetailsRequest", corp_msg.clone()),
        ("eve.corporation.title.GetAllRequest", corp_msg.clone()),
        ("eve.corporation.vote.GetRequest", seq_id_msg(1, 902)),
        ("eve.corporation.starbase.GetAllRequest", corp_msg.clone()),
        ("eve.corporation.starbase.GetSettingsAndFuelsRequest", corp_msg.clone()),
        ("eve.corporation.medals.GetRequest", corp_msg.clone()),
        ("eve.corporation.medals.issued.GetRequest", corp_msg.clone()),
        ("eve.corporation.market.entry.GetEntriesRequest", corp_msg.clone()),
        ("eve.corporation.standing.npc.GetAllRequest", corp_msg.clone()),
        ("eve.corporation.planetinteraction.office.GetRequest", corp_msg),
    ];

    for (req_name, req_msg) in cases {
        let resp = handlers[req_name](&auth_ctx, &dummy_any, &req_msg)
            .unwrap_or_else(|e| panic!("handler {req_name} failed: {e:?}"));
        let decoded = reg
            .decode_dynamic(&resp.type_url, &resp.value)
            .unwrap_or_else(|e| panic!("strict decode failed for {}: {e}", resp.type_url));
        assert_eq!(
            reg.encode_dynamic(&decoded).unwrap(),
            resp.value,
            "roundtrip mismatch for {}",
            resp.type_url
        );
    }
}

#[cfg(test)]
#[path = "quasar_corp_domain_extra_unit_tests.rs"]
mod quasar_corp_domain_extra_unit_tests;
