//! Additional branch-coverage and mutation-killing unit tests for `quasar_social_freelance`.

#![forbid(unsafe_code)]

use super::quasar_social_freelance::*;
use super::social_corp_fleet::social_corp_projects;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::durable_domain_types::{CorpGovState, PersistedProjectDetails};
use eve_db::{corp_roles, CorpProjectRecord, IskCents, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn uuid_msg(field: u32, uuid: &[u8]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer
        .fields
        .insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn make_matcher_msg(num_str: &str) -> DynamicQuasarMessage {
    let mut vem = DynamicQuasarMessage::new("Vem");
    vem.fields.insert(
        2,
        DynamicValue::Repeated(vec![DynamicValue::String(num_str.into())]),
    );
    let mut mm = DynamicQuasarMessage::new("Mm");
    mm.fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(vem))]));
    mm
}

fn make_currency_msg(units: i64, nanos: i32) -> DynamicQuasarMessage {
    let mut cur = DynamicQuasarMessage::new("Currency");
    cur.fields.insert(1, DynamicValue::Int64(units));
    cur.fields.insert(2, DynamicValue::Int32(nanos));
    cur
}

fn make_corp_proj(id: i64, name: &str, target: u64, cur: u64, unit_isk: i64, escrow_isk: i64) -> CorpProjectRecord {
    CorpProjectRecord {
        project_id: id,
        corporation_id: 1_000_009,
        name: name.into(),
        target_quantity: target,
        current_quantity: cur,
        isk_per_unit: IskCents::from_isk(unit_isk),
        escrow_remaining: IskCents::from_isk(escrow_isk),
    }
}

#[test]
fn build_freelance_map_states_contributors_and_serde_defaults() {
    let json = r#"{
        "uuid": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],
        "owner_corp_id": 1000009,
        "creator_char_id": 90000001,
        "name": "Serde Default Test",
        "description": "Desc",
        "career": 1,
        "deadline": 1000,
        "created": 500,
        "target_progress": 100,
        "current_progress": 10,
        "state": 1,
        "committed_participants": [],
        "contributors": {},
        "reward_isk": 5000
    }"#;
    let rec: FreelanceProjectRecord = serde_json::from_str(json).expect("deserialize");
    assert_eq!(rec.required_type_id, 34);
    assert_eq!(rec.batch_size, 5);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let u_active = [0xAA; 16];
    let u_completed = [0xBB; 16];
    let u_closed = [0xCC; 16];

    db.with_mut_snapshot(|snap| {
        snap.corp_projects.insert(501, make_corp_proj(501, "NonFreelance", 10, 0, 0, 0));
        snap.alliance_war_state
            .project_meta
            .insert(501, ("DestroyNPCShips".into(), false, false));

        snap.corp_projects.insert(502, make_corp_proj(502, "ActiveProj", 100, 25, 100, 7_500));
        snap.alliance_war_state
            .project_meta
            .insert(502, ("DeliverItem".into(), true, false));
        let char_contribs = HashMap::from([(0, 50), (-5, 50), (90_000_001, 25)]);
        let redeemed = HashMap::from([(90_000_001, 2)]);
        snap.alliance_war_state.project_details.insert(
            502,
            PersistedProjectDetails {
                uuid_bytes: u_active.to_vec(),
                creator_id: 90_000_001,
                description: "Deliver Trit".into(),
                status: "Active".into(),
                batch_size: 5,
                filter_id: Some(34),
                char_contributions: char_contribs,
                contributions_by_char: redeemed,
                committed_participants: vec![90_000_001, -1],
                ..Default::default()
            },
        );

        snap.corp_projects.insert(503, make_corp_proj(503, "DoneProj", 10, 10, 0, 0));
        snap.alliance_war_state
            .project_meta
            .insert(503, ("MineOre".into(), true, false));
        snap.alliance_war_state.project_details.insert(
            503,
            PersistedProjectDetails {
                uuid_bytes: u_completed.to_vec(),
                status: "Active".into(),
                ..Default::default()
            },
        );

        snap.corp_projects.insert(504, make_corp_proj(504, "ClosedProj", 10, 2, 0, 0));
        snap.alliance_war_state
            .project_meta
            .insert(504, ("MineOre".into(), true, false));
        snap.alliance_war_state.project_details.insert(
            504,
            PersistedProjectDetails {
                uuid_bytes: u_closed.to_vec(),
                closed: true,
                ..Default::default()
            },
        );

        snap.corp_projects.insert(505, make_corp_proj(505, "ExpiredProj", 10, 1, 0, 0));
        snap.alliance_war_state
            .project_meta
            .insert(505, ("MineOre".into(), true, false));
        snap.alliance_war_state.project_details.insert(
            505,
            PersistedProjectDetails {
                uuid_bytes: Vec::new(),
                created_unix: 1_700_000_000,
                expired: true,
                ..Default::default()
            },
        );
    });

    let state = QuasarFreelanceState::from_db(Arc::clone(&db));
    state.sync_to_db();
    state.refresh_from_db(1_700_000_100);

    let projs = state.projects.read().unwrap();
    assert_eq!(projs.len(), 4);
    let act = &projs[&u_active.to_vec()];
    assert_eq!(act.state, 1);
    assert_eq!(act.reward_isk, 7_500);
    assert_eq!(act.required_type_id, 34);
    assert_eq!(act.batch_size, 5);
    assert_eq!(act.committed_participants, vec![90_000_001]);
    assert_eq!(act.contributors.get(&90_000_001), Some(&(25, 3)));
    assert!(!act.contributors.contains_key(&0));

    assert_eq!(projs[&u_completed.to_vec()].state, 2);
    assert_eq!(projs[&u_closed.to_vec()].state, 3);
    let u_exp = social_corp_projects::freelance_uuid(1_700_000_000, 505).to_vec();
    assert_eq!(projs[&u_exp].state, 4);
}

#[test]
fn create_request_validation_parameter_extraction_and_budget_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    register(&router, Arc::clone(&db));
    let handlers = router.handlers.read().unwrap();
    let create_fn = &handlers["eve_public.freelance.project.api.CreateRequest"];
    let any = GoogleAny::default();

    let dir_cid = 90_000_001_u32;
    let pm_cid = 90_000_002_u32;
    let corp_id = 1_000_009_u32;
    db.with_mut_snapshot(|snap| {
        let c = snap.corporations.entry(corp_id as i64).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id as i64;
            __corp.ceo_id = dir_cid as i64;
            __corp });
        c.set_division_balance(1, IskCents::from_isk(100_000_000));
        c.set_division_balance(1, IskCents::from_isk(100_000_000));
        c.set_division_balance(3, IskCents::from_isk(100_000_000));
        for id in [dir_cid as i64, pm_cid as i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id as i64;
            }
        }
        let mut gov = CorpGovState::default();
        gov.member_account_keys.insert(pm_cid as i64, 1002); // wallet_div = 3
        snap.corp_governance.insert(corp_id as i64, gov);
    });

    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    assert_eq!(
        create_fn(&unauth_ctx, &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    let no_corp_ctx =
        AuthoritativeContext::for_session(Some(1), Some(dir_cid), None, 0, "tranquility", vec![]);
    assert_eq!(
        create_fn(&no_corp_ctx, &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(dir_cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![],
    );

    assert_eq!(
        create_fn(&dir_ctx, &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let mut blank_name = DynamicQuasarMessage::new("Req");
    blank_name.fields.insert(1, DynamicValue::String("   ".into()));
    assert_eq!(create_fn(&dir_ctx, &any, &blank_name).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut bad_target = DynamicQuasarMessage::new("Req");
    bad_target.fields.insert(1, DynamicValue::String("Valid Name".into()));
    bad_target.fields.insert(3, DynamicValue::Int64(0));
    assert_eq!(create_fn(&dir_ctx, &any, &bad_target).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut bad_item_req = DynamicQuasarMessage::new("Req");
    bad_item_req.fields.insert(1, DynamicValue::String("Bad Item".into()));
    bad_item_req.fields.insert(3, DynamicValue::Int64(10));
    assert_eq!(create_fn(&dir_ctx, &any, &bad_item_req).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut m_sub = DynamicQuasarMessage::new("MSub");
    m_sub.fields.insert(1, DynamicValue::String("DeliverItem".into()));
    let mut om = DynamicQuasarMessage::new("Om");
    om.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::String("34".into())]));
    let mut pm1 = DynamicQuasarMessage::new("Pm1");
    pm1.fields.insert(3, DynamicValue::Message(Box::new(om)));

    let mut cm = DynamicQuasarMessage::new("Cm");
    cm.fields.insert(1, DynamicValue::Message(Box::new(make_matcher_msg("5"))));
    let mut pm2 = DynamicQuasarMessage::new("Pm2");
    pm2.fields.insert(5, DynamicValue::Message(Box::new(cm)));

    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::Message(Box::new(m_sub)));
    cfg.fields.insert(
        3,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(pm1)),
            DynamicValue::Message(Box::new(pm2)),
        ]),
    );

    let mut mods = DynamicQuasarMessage::new("Mods");
    mods.fields.insert(2, DynamicValue::Int64(50));

    let mut rew = DynamicQuasarMessage::new("Rew");
    rew.fields.insert(1, DynamicValue::Message(Box::new(make_currency_msg(100, 0))));
    let mut ec = DynamicQuasarMessage::new("Ec");
    ec.fields.insert(3, DynamicValue::Message(Box::new(make_currency_msg(2_000, 0))));
    let mut dl = DynamicQuasarMessage::new("Dl");
    dl.fields.insert(1, DynamicValue::Int64(1_900_000_000));

    let mut deliver_req = DynamicQuasarMessage::new("Req");
    deliver_req.fields.insert(1, DynamicValue::String("Trit Delivery".into()));
    deliver_req.fields.insert(2, DynamicValue::String("Bring Tritanium".into()));
    deliver_req.fields.insert(3, DynamicValue::Int64(20));
    deliver_req.fields.insert(5, DynamicValue::Message(Box::new(dl)));
    deliver_req.fields.insert(6, DynamicValue::Message(Box::new(cfg)));
    deliver_req.fields.insert(7, DynamicValue::Message(Box::new(mods)));
    deliver_req.fields.insert(10, DynamicValue::Message(Box::new(rew)));
    deliver_req.fields.insert(11, DynamicValue::Message(Box::new(ec)));

    let pm_no_take = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_cid),
        Some(corp_id),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![],
    );
    assert_eq!(create_fn(&pm_no_take, &any, &deliver_req).unwrap_err().0, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&(corp_id as i64)) {
            c.member_roles
                .insert(pm_cid as i64, corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]);
        }
    });
    let pm_with_take = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_cid),
        Some(corp_id),
        corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
        "tranquility",
        vec![],
    );
    assert!(create_fn(&pm_with_take, &any, &deliver_req).is_ok());

    let mut pm_m2 = DynamicQuasarMessage::new("PmM2");
    pm_m2.fields.insert(2, DynamicValue::Message(Box::new(make_matcher_msg("30000142"))));
    let mut cfg2 = DynamicQuasarMessage::new("Cfg2");
    cfg2.fields.insert(1, DynamicValue::String("DestroyNPCShips".into()));
    cfg2.fields.insert(3, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(pm_m2))]));
    let mut dep_only = DynamicQuasarMessage::new("Req");
    dep_only.fields.insert(1, DynamicValue::String("Deposit Only".into()));
    dep_only.fields.insert(3, DynamicValue::Uint64(10));
    dep_only.fields.insert(6, DynamicValue::Message(Box::new(cfg2)));
    let mut ec2 = DynamicQuasarMessage::new("Ec2");
    ec2.fields.insert(4, DynamicValue::Message(Box::new(make_currency_msg(5_000, 0))));
    dep_only.fields.insert(11, DynamicValue::Message(Box::new(ec2)));
    assert!(create_fn(&dir_ctx, &any, &dep_only).is_ok());
}

#[test]
fn query_admin_kick_and_redeem_reward_error_and_success_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    register(&router, Arc::clone(&db));
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();

    let dir_cid = 90_000_001_u32;
    let worker_cid = 90_000_002_u32;
    let corp_id = 1_000_009_u32;
    let other_corp = 1_000_010_u32;

    db.with_mut_snapshot(|snap| {
        let c = snap.corporations.entry(corp_id as i64).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id as i64;
            __corp.ceo_id = dir_cid as i64;
            __corp });
        c.set_division_balance(1, IskCents::from_isk(50_000_000));
        c.set_division_balance(1, IskCents::from_isk(50_000_000));
        for id in [dir_cid as i64, worker_cid as i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id as i64;
            }
        }
    });

    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(dir_cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![],
    );
    let worker_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(worker_cid),
        Some(corp_id),
        0,
        "tranquility",
        vec![],
    );
    let other_corp_dir = AuthoritativeContext::for_session(
        Some(3),
        Some(dir_cid),
        Some(other_corp),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![],
    );

    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::String("DestroyNPCShips".into()));
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Reward Project".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(10));
    create_msg.fields.insert(6, DynamicValue::Message(Box::new(cfg)));
    create_msg.fields.insert(10, DynamicValue::Message(Box::new(make_currency_msg(500, 0))));
    handlers["eve_public.freelance.project.api.CreateRequest"](&dir_ctx, &any, &create_msg)
        .expect("create reward project");

    let (pid, proj_uuid) = db.with_snapshot(|snap| {
        let (&pid, _) = snap.alliance_war_state.project_meta.iter().find(|(_, m)| m.1).unwrap();
        (pid, snap.alliance_war_state.project_details[&pid].uuid_bytes.clone())
    });

    db.with_mut_snapshot(|snap| {
        if let Some(p) = snap.corp_projects.get_mut(&pid) {
            p.current_quantity = 4;
        }
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
            det.committed_participants.push(worker_cid as i64);
            det.char_contributions.insert(worker_cid as i64, 4);
        }
    });

    for ep in [
        "eve_public.freelance.project.api.GetRequest",
        "eve_public.freelance.project.api.GetForManagerRequest",
        "eve_public.freelance.project.api.GetParticipationDetailsRequest",
    ] {
        assert_eq!(handlers[ep](&worker_ctx, &any, &DynamicQuasarMessage::new("Empty")).unwrap_err().0, STATUS_BAD_REQUEST);
        assert_eq!(handlers[ep](&worker_ctx, &any, &uuid_msg(1, &[0xFF; 16])).unwrap_err().0, STATUS_NOT_FOUND);
    }

    assert_eq!(
        handlers["eve_public.freelance.project.api.GetAllActiveForCorporationRequest"](
            &unauth_ctx,
            &any,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let mut corp_sub = DynamicQuasarMessage::new("CorpId");
    corp_sub.fields.insert(1, DynamicValue::Int32(corp_id as i32));
    let mut corp_req = DynamicQuasarMessage::new("Req");
    corp_req.fields.insert(1, DynamicValue::Message(Box::new(corp_sub)));
    assert!(
        handlers["eve_public.freelance.project.api.GetAllActiveForCorporationRequest"](
            &unauth_ctx,
            &any,
            &corp_req
        )
        .is_ok()
    );

    let mut bcast_req = DynamicQuasarMessage::new("Req");
    bcast_req.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Int64(30_000_142)]));
    assert!(handlers["eve_public.freelance.project.api.GetAllBroadcastedRequest"](&worker_ctx, &any, &bcast_req).is_ok());

    let mut stats_req = DynamicQuasarMessage::new("Req");
    stats_req.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Bytes(proj_uuid.clone())]));
    let stats_resp = handlers["eve_public.freelance.project.api.GetStatsRequest"](&worker_ctx, &any, &stats_req).unwrap();
    assert!(!stats_resp.value.is_empty());

    assert_eq!(
        handlers["eve_public.freelance.project.api.GetAllUnredeemedRequest"](
            &unauth_ctx,
            &any,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let unred_resp = handlers["eve_public.freelance.project.api.GetAllUnredeemedRequest"](
        &worker_ctx,
        &any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!unred_resp.value.is_empty());

    assert_eq!(
        handlers["eve_public.freelance.project.api.RedeemRewardRequest"](
            &worker_ctx,
            &any,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.freelance.project.api.RedeemRewardRequest"](&unauth_ctx, &any, &uuid_msg(1, &proj_uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.freelance.project.api.RedeemRewardRequest"](&worker_ctx, &any, &uuid_msg(1, &[0xEE; 16]))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert!(
        handlers["eve_public.freelance.project.api.RedeemRewardRequest"](&worker_ctx, &any, &uuid_msg(1, &proj_uuid))
            .is_ok()
    );

    let mut kick_msg = uuid_msg(1, &proj_uuid);
    assert_eq!(
        handlers["eve_public.freelance.project.api.KickRequest"](&dir_ctx, &any, &kick_msg).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    kick_msg.fields.insert(2, DynamicValue::Uint32(worker_cid));
    assert_eq!(
        handlers["eve_public.freelance.project.api.KickRequest"](&other_corp_dir, &any, &kick_msg).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.freelance.project.api.CloseRequest"](&other_corp_dir, &any, &uuid_msg(1, &proj_uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let mut set_name_bad = uuid_msg(1, &proj_uuid);
    set_name_bad.fields.insert(2, DynamicValue::String("   ".into()));
    assert_eq!(
        handlers["eve.freelance.project.api.admin.SetNameRequest"](&dir_ctx, &any, &set_name_bad).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    set_name_bad.fields.insert(2, DynamicValue::String("Valid".into()));
    assert_eq!(
        handlers["eve.freelance.project.api.admin.SetNameRequest"](&other_corp_dir, &any, &set_name_bad).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve.freelance.project.api.admin.SetDescriptionRequest"](
            &dir_ctx,
            &any,
            &uuid_msg(1, &proj_uuid)
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}
