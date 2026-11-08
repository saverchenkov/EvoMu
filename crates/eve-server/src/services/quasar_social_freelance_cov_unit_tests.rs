//! Comprehensive branch-coverage and mutation-killing unit tests for `quasar_social_freelance` (`Q-5`).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::durable_domain_types::{CorpGovState, PersistedProjectDetails};
use eve_db::{corp_roles, CorpProjectRecord, CorporationRecord, IskCents, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn uuid_req(field: u32, uuid: &[u8]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Id");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn matcher_msg(num_str: &str) -> DynamicQuasarMessage {
    let mut vem = DynamicQuasarMessage::new("Vem");
    vem.fields.insert(
        2,
        DynamicValue::Repeated(vec![
            DynamicValue::Int64(1),
            DynamicValue::String("not-a-u32".into()),
            DynamicValue::String(num_str.into()),
        ]),
    );
    let mut mm = DynamicQuasarMessage::new("Mm");
    mm.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Int64(0), DynamicValue::Message(Box::new(vem))]));
    mm
}

fn currency_msg(units: i64) -> DynamicQuasarMessage {
    let mut cur = DynamicQuasarMessage::new("Currency");
    cur.fields.insert(1, DynamicValue::Int64(units));
    cur
}

fn corp_proj(id: i64, corp_id: i64, name: &str, target: u64, cur: u64, unit_isk: i64, escrow_isk: i64) -> CorpProjectRecord {
    CorpProjectRecord {
        project_id: id,
        corporation_id: corp_id,
        name: name.into(),
        target_quantity: target,
        current_quantity: cur,
        isk_per_unit: IskCents::from_isk(unit_isk),
        escrow_remaining: IskCents::from_isk(escrow_isk),
    }
}

fn sess(uid: Option<i32>, cid: Option<u32>, corp: Option<u32>, roles: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session(uid, cid, corp, roles, "tranquility", vec![])
}

#[test]
fn serde_defaults_snapshot_mapping_admin_role_divisions_and_extractors() {
    let raw_json = r#"{"uuid":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"owner_corp_id":1000009,"creator_char_id":90000001,"name":"Legacy","description":"Desc","career":1,"deadline":2000,"created":1000,"target_progress":50,"current_progress":5,"state":1,"committed_participants":[],"contributors":{},"reward_isk":1000}"#;
    let rec: FreelanceProjectRecord = serde_json::from_str(raw_json).expect("serde defaults");
    assert_eq!((rec.required_type_id, rec.batch_size, rec.initial_reward_isk), (34, 5, None));

    assert_eq!(extract_uuid(None), None);
    assert_eq!(extract_uuid(Some(&DynamicValue::Int64(7))), None);
    assert_eq!(extract_uuid(Some(&DynamicValue::Bytes(vec![7, 8]))), Some(vec![7, 8]));
    assert_eq!(extract_id(None), None);
    assert_eq!(extract_id(Some(&DynamicValue::String("x".into()))), None);
    assert_eq!(extract_id(Some(&DynamicValue::Uint64(11))), Some(11));
    assert_eq!(extract_id(Some(&DynamicValue::Int64(12))), Some(12));
    assert_eq!(extract_id(Some(&DynamicValue::Uint32(13))), Some(13));
    assert_eq!(extract_id(Some(&DynamicValue::Int32(14))), Some(14));
    for (v, expected) in [
        (DynamicValue::Uint64(21), Some(21)),
        (DynamicValue::Int64(22), Some(22)),
        (DynamicValue::Uint32(23), Some(23)),
        (DynamicValue::Int32(24), Some(24)),
        (DynamicValue::String("bad".into()), None),
    ] {
        let mut sub = DynamicQuasarMessage::new("Sub");
        sub.fields.insert(1, v);
        assert_eq!(extract_id(Some(&DynamicValue::Message(Box::new(sub)))), expected);
    }

    let db = Arc::new(WorldRepository::with_demo_seed());
    let (u_act, u_done_qty, u_done_str, u_closed_str, u_exp_str) =
        ([0xA1; 16], [0xA2; 16], [0xA3; 16], [0xA4; 16], [0xA5; 16]);

    db.with_mut_snapshot(|snap| {
        snap.corp_projects.insert(601, corp_proj(601, 1_000_009, "GoalOnly", 10, 0, 0, 0));
        snap.alliance_war_state.project_meta.insert(601, ("MineOre".into(), false, false));

        snap.corp_projects.insert(602, corp_proj(602, 1_000_009, "Active", 100, 25, 100, 4_000));
        snap.alliance_war_state.project_meta.insert(602, ("DeliverItem".into(), true, false));
        snap.alliance_war_state.project_details.insert(
            602,
            PersistedProjectDetails {
                uuid_bytes: u_act.to_vec(),
                creator_id: 90_000_001,
                created_unix: 1_000,
                expires_unix: None,
                status: "Active".into(),
                batch_size: 5,
                filter_id: Some(34),
                initial_escrow: IskCents::from_isk(10_000),
                char_contributions: HashMap::from([(0, 50), (-2, 10), (90_000_001, 25), (90_000_002, 10)]),
                contributions_by_char: HashMap::from([(90_000_001, 2), (90_000_002, 9)]),
                committed_participants: vec![90_000_001, -9],
                ..Default::default()
            },
        );

        for (pid, name, tgt, cur, esc, det) in [
            (603, "DoneQty", 10, 10, 500, PersistedProjectDetails { uuid_bytes: u_done_qty.to_vec(), expires_unix: Some(9_999), status: "Active".into(), ..Default::default() }),
            (604, "DoneStr", 10, 2, 0, PersistedProjectDetails { uuid_bytes: u_done_str.to_vec(), status: "Completed".into(), ..Default::default() }),
            (605, "ClosedStr", 10, 2, 0, PersistedProjectDetails { uuid_bytes: u_closed_str.to_vec(), status: "Closed".into(), ..Default::default() }),
            (606, "ExpStr", 10, 2, 0, PersistedProjectDetails { uuid_bytes: u_exp_str.to_vec(), status: "Expired".into(), ..Default::default() }),
            (607, "GenUuidExp", 10, 2, 0, PersistedProjectDetails { uuid_bytes: vec![1, 2, 3], created_unix: 1_700_000_000, expired: true, ..Default::default() }),
        ] {
            snap.corp_projects.insert(pid, corp_proj(pid, 1_000_009, name, tgt, cur, 0, esc));
            snap.alliance_war_state.project_meta.insert(pid, ("MineOre".into(), true, false));
            snap.alliance_war_state.project_details.insert(pid, det);
        }
    });

    let (map, next_seq) = db.with_snapshot(build_freelance_map_from_snap);
    assert_eq!((next_seq, map.len()), (608, 6));

    let act = &map[&u_act.to_vec()];
    assert_eq!((act.state, act.deadline, act.reward_isk, act.initial_reward_isk), (1, 1_000 + 604_800, 4_000, Some(10_000)));
    assert_eq!(act.committed_participants, vec![90_000_001]);
    assert_eq!(act.contributors.get(&90_000_001), Some(&(25, 3)));
    assert_eq!(act.contributors.get(&90_000_002), Some(&(10, 0)));
    assert!(!encode_project_attributes(act).is_empty());

    let done_qty = &map[&u_done_qty.to_vec()];
    assert_eq!((done_qty.state, done_qty.deadline, done_qty.initial_reward_isk), (2, 9_999, Some(500)));
    assert_eq!(map[&u_done_str.to_vec()].state, 2);
    assert_eq!(map[&u_closed_str.to_vec()].state, 3);
    assert_eq!(map[&u_exp_str.to_vec()].state, 4);
    assert_eq!(map[&corp_projects::freelance_uuid(1_700_000_000, 607).to_vec()].state, 4);

    let (ceo_cid, pm_cid, corp_id) = (90_000_001_u32, 90_000_002_u32, 98_500_001_u32);
    let ctx_pm = sess(Some(1), Some(pm_cid), Some(corp_id), corp_roles::ROLE_PROJECT_MANAGER);
    assert_eq!(check_corp_admin_role(&ctx_pm, None, pm_cid, corp_id).unwrap(), (false, corp_roles::ROLE_PROJECT_MANAGER, 1));

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id as i64, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id as i64;
            __corp.ceo_id = ceo_cid as i64;
            __corp });
    });
    let ctx_ceo_zero = sess(Some(1), Some(ceo_cid), Some(corp_id), 0);
    assert_eq!(check_corp_admin_role(&ctx_ceo_zero, Some(&db), ceo_cid, corp_id).unwrap(), (true, 0, 1));
    let ctx_member = sess(Some(2), Some(pm_cid), Some(corp_id), 0);
    assert_eq!(check_corp_admin_role(&ctx_member, Some(&db), pm_cid, corp_id).unwrap_err().0, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&(corp_id as i64)) {
            c.members.insert(pm_cid as i64);
            c.member_roles.insert(pm_cid as i64, corp_roles::ROLE_PROJECT_MANAGER);
        }
    });
    for (acct, expected_div) in [(999, 1u8), (1000, 1), (1003, 4), (1006, 7), (1007, 1)] {
        db.with_mut_snapshot(|snap| {
            snap.corp_governance.entry(corp_id as i64).or_default().member_account_keys.insert(pm_cid as i64, acct);
        });
        assert_eq!(check_corp_admin_role(&ctx_pm, Some(&db), pm_cid, corp_id).unwrap().2, expected_div);
    }
}

#[test]
fn create_request_config_variants_negative_budget_and_escrow_role_checks() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(Arc::new(QuasarDescriptorRegistry::empty_with_builtins()), Arc::clone(&db), shared_sde());
    register(&router, Arc::clone(&db));
    let handlers = router.handlers.read().unwrap();
    let create_fn = &handlers["eve_public.freelance.project.api.CreateRequest"];
    let any = GoogleAny::default();

    let (dir_cid, pm_cid, corp_id) = (90_000_001_u32, 90_000_002_u32, 1_000_009_u32);
    db.with_mut_snapshot(|snap| {
        let c = snap.corporations.entry(corp_id as i64).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id as i64;
            __corp.ceo_id = dir_cid as i64;
            __corp });
        c.members.extend([dir_cid as i64, pm_cid as i64]);
        c.set_division_balance(1, IskCents::from_isk(200_000_000));
        c.set_division_balance(1, IskCents::from_isk(200_000_000));
        c.set_division_balance(4, IskCents::from_isk(200_000_000));
        c.member_roles.insert(pm_cid as i64, corp_roles::ROLE_PROJECT_MANAGER);
        let mut gov = CorpGovState::default();
        gov.member_account_keys.insert(pm_cid as i64, 1003);
        snap.corp_governance.insert(corp_id as i64, gov);
    });

    assert_eq!(create_fn(&sess(None, None, None, 0), &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(create_fn(&sess(Some(1), Some(dir_cid), None, 0), &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0, STATUS_FORBIDDEN);

    let dir_ctx = sess(Some(1), Some(dir_cid), Some(corp_id), corp_roles::ROLE_DIRECTOR);
    assert_eq!(create_fn(&dir_ctx, &any, &DynamicQuasarMessage::new("Req")).unwrap_err().0, STATUS_BAD_REQUEST);
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.fields.insert(1, DynamicValue::String("   ".into()));
    assert_eq!(create_fn(&dir_ctx, &any, &msg).unwrap_err().0, STATUS_BAD_REQUEST);
    msg.fields.insert(1, DynamicValue::String("Valid".into()));
    msg.fields.insert(3, DynamicValue::Int64(0));
    assert_eq!(create_fn(&dir_ctx, &any, &msg).unwrap_err().0, STATUS_BAD_REQUEST);
    msg.fields.insert(3, DynamicValue::Int64(10));
    assert_eq!(create_fn(&dir_ctx, &any, &msg).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut om_unknown = DynamicQuasarMessage::new("Om");
    om_unknown.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::String("999999999".into())]));
    let mut pm_unknown = DynamicQuasarMessage::new("Pm");
    pm_unknown.fields.insert(3, DynamicValue::Message(Box::new(om_unknown)));
    let mut cfg_unknown = DynamicQuasarMessage::new("Cfg");
    cfg_unknown.fields.insert(3, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(pm_unknown))]));
    msg.fields.insert(6, DynamicValue::Message(Box::new(cfg_unknown)));
    assert_eq!(create_fn(&dir_ctx, &any, &msg).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut cfg_tracked = DynamicQuasarMessage::new("Cfg");
    cfg_tracked.fields.insert(1, DynamicValue::String("MineOre".into()));
    let mut neg_budget = DynamicQuasarMessage::new("Req");
    neg_budget.fields.insert(1, DynamicValue::String("Neg Budget".into()));
    neg_budget.fields.insert(3, DynamicValue::Uint64(5));
    neg_budget.fields.insert(6, DynamicValue::Message(Box::new(cfg_tracked)));
    neg_budget.fields.insert(10, DynamicValue::Int64(-50));
    assert_eq!(create_fn(&dir_ctx, &any, &neg_budget).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut m_sub = DynamicQuasarMessage::new("MSub");
    m_sub.fields.insert(1, DynamicValue::String("DeliverItem".into()));
    let mut om = DynamicQuasarMessage::new("Om");
    om.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Int64(0), DynamicValue::String("34".into())]));
    let mut pm1 = DynamicQuasarMessage::new("Pm1");
    pm1.fields.insert(3, DynamicValue::Message(Box::new(om)));
    let mut cm = DynamicQuasarMessage::new("Cm");
    cm.fields.insert(1, DynamicValue::Message(Box::new(matcher_msg("5"))));
    let mut pm2 = DynamicQuasarMessage::new("Pm2");
    pm2.fields.insert(5, DynamicValue::Message(Box::new(cm)));

    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::Message(Box::new(m_sub)));
    cfg.fields.insert(3, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(pm1)), DynamicValue::Message(Box::new(pm2))]));
    let mut mods = DynamicQuasarMessage::new("Mods");
    mods.fields.insert(2, DynamicValue::Uint64(40));
    let mut dl = DynamicQuasarMessage::new("Dl");
    dl.fields.insert(1, DynamicValue::Sint64(1_850_000_000));
    let mut rew = DynamicQuasarMessage::new("Rew");
    rew.fields.insert(1, DynamicValue::Message(Box::new(currency_msg(100))));
    let mut ec = DynamicQuasarMessage::new("Ec");
    ec.fields.insert(3, DynamicValue::Message(Box::new(currency_msg(2_000))));

    let mut deliver_req = DynamicQuasarMessage::new("Req");
    deliver_req.fields.insert(1, DynamicValue::String("Trit Supply".into()));
    deliver_req.fields.insert(2, DynamicValue::String("Supply Tritanium".into()));
    deliver_req.fields.insert(3, DynamicValue::Int64(20));
    deliver_req.fields.insert(4, DynamicValue::Message(Box::new(dl)));
    deliver_req.fields.insert(6, DynamicValue::Message(Box::new(cfg)));
    deliver_req.fields.insert(7, DynamicValue::Message(Box::new(mods)));
    deliver_req.fields.insert(10, DynamicValue::Message(Box::new(rew)));
    deliver_req.fields.insert(11, DynamicValue::Message(Box::new(ec)));

    let pm_denied = sess(Some(2), Some(pm_cid), Some(corp_id), corp_roles::ROLE_PROJECT_MANAGER);
    assert_eq!(create_fn(&pm_denied, &any, &deliver_req).unwrap_err().0, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&(corp_id as i64)) {
            c.member_roles.insert(pm_cid as i64, corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[3]);
        }
    });
    assert!(create_fn(&pm_denied, &any, &deliver_req).is_ok());

    let mut pm_m2 = DynamicQuasarMessage::new("PmM2");
    pm_m2.fields.insert(2, DynamicValue::Message(Box::new(matcher_msg("30000142"))));
    let mut cfg2 = DynamicQuasarMessage::new("Cfg2");
    cfg2.fields.insert(1, DynamicValue::String("DestroyNPCShips".into()));
    cfg2.fields.insert(3, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(pm_m2))]));
    let mut mods2 = DynamicQuasarMessage::new("Mods2");
    mods2.fields.insert(2, DynamicValue::Int64(15));
    let mut dl2 = DynamicQuasarMessage::new("Dl2");
    dl2.fields.insert(1, DynamicValue::Uint64(1_950_000_000));
    let mut ec2 = DynamicQuasarMessage::new("Ec2");
    ec2.fields.insert(4, DynamicValue::Message(Box::new(currency_msg(5_000))));

    let mut dep_only = DynamicQuasarMessage::new("Req");
    dep_only.fields.insert(1, DynamicValue::String("Ratting Bounty".into()));
    dep_only.fields.insert(3, DynamicValue::Uint64(10));
    dep_only.fields.insert(5, DynamicValue::Message(Box::new(dl2)));
    dep_only.fields.insert(6, DynamicValue::Message(Box::new(cfg2)));
    dep_only.fields.insert(7, DynamicValue::Message(Box::new(mods2)));
    dep_only.fields.insert(11, DynamicValue::Message(Box::new(ec2)));
    assert!(create_fn(&dir_ctx, &any, &dep_only).is_ok());
}

#[test]
fn full_lifecycle_queries_close_escrow_guard_delete_and_redeem_reward() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(Arc::new(QuasarDescriptorRegistry::empty_with_builtins()), Arc::clone(&db), shared_sde());
    register(&router, Arc::clone(&db));
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();

    let (dir_cid, pm_cid, worker_cid, corp_id, other_corp) =
        (90_000_001_u32, 90_000_002_u32, 90_000_003_u32, 1_000_009_u32, 1_000_010_u32);

    db.with_mut_snapshot(|snap| {
        let mut w_ch = snap.characters[&(dir_cid as i64)].clone();
        w_ch.character_id = worker_cid as i64;
        w_ch.corporation_id = corp_id as i64;
        snap.characters.insert(worker_cid as i64, w_ch);
        let c = snap.corporations.entry(corp_id as i64).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id as i64;
            __corp.ceo_id = dir_cid as i64;
            __corp });
        c.set_division_balance(1, IskCents::from_isk(80_000_000));
        c.set_division_balance(1, IskCents::from_isk(80_000_000));
        c.member_roles.insert(pm_cid as i64, corp_roles::ROLE_PROJECT_MANAGER);
    });

    let unauth = sess(None, None, None, 0);
    let dir_ctx = sess(Some(1), Some(dir_cid), Some(corp_id), corp_roles::ROLE_DIRECTOR);
    let pm_no_take = sess(Some(2), Some(pm_cid), Some(corp_id), corp_roles::ROLE_PROJECT_MANAGER);
    let worker_ctx = sess(Some(3), Some(worker_cid), Some(corp_id), 0);
    let other_corp_dir = sess(Some(4), Some(dir_cid), Some(other_corp), corp_roles::ROLE_DIRECTOR);

    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::String("DestroyNPCShips".into()));
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Funded Patrol".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(10));
    create_msg.fields.insert(6, DynamicValue::Message(Box::new(cfg)));
    create_msg.fields.insert(10, DynamicValue::Message(Box::new(currency_msg(500))));
    handlers["eve_public.freelance.project.api.CreateRequest"](&dir_ctx, &any, &create_msg).unwrap();

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
        snap.corp_projects.insert(777, corp_proj(777, corp_id as i64, "OldClosed", 5, 0, 0, 0));
        snap.alliance_war_state.project_meta.insert(777, ("MineOre".into(), true, false));
        snap.alliance_war_state.project_details.insert(777, PersistedProjectDetails { uuid_bytes: [0xD1; 16].to_vec(), closed: true, ..Default::default() });
    });

    for ep in [
        "eve_public.freelance.project.api.GetRequest",
        "eve_public.freelance.project.api.GetForManagerRequest",
        "eve_public.freelance.project.api.GetParticipationDetailsRequest",
    ] {
        assert_eq!(handlers[ep](&worker_ctx, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
        assert_eq!(handlers[ep](&worker_ctx, &any, &uuid_req(1, &[0xFF; 16])).unwrap_err().0, STATUS_NOT_FOUND);
        if ep == "eve_public.freelance.project.api.GetRequest" {
            assert!(handlers[ep](&worker_ctx, &any, &uuid_req(1, &proj_uuid)).is_ok());
        } else {
            assert_eq!(handlers[ep](&worker_ctx, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
            assert!(handlers[ep](&dir_ctx, &any, &uuid_req(1, &proj_uuid)).is_ok());
        }
    }
    assert_eq!(handlers["eve_public.freelance.project.api.GetAllActiveForCorporationRequest"](&unauth, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_FORBIDDEN);
    let mut corp_req = DynamicQuasarMessage::new("Req");
    corp_req.fields.insert(1, DynamicValue::Uint32(corp_id));
    assert!(handlers["eve_public.freelance.project.api.GetAllActiveForCorporationRequest"](&unauth, &any, &corp_req).is_ok());

    let inact = handlers["eve_public.freelance.project.api.GetAllInactiveRequest"](&worker_ctx, &any, &DynamicQuasarMessage::new("E")).unwrap();
    assert!(!WireMessage::decode(&inact.value).unwrap().fields.is_empty());

    let mut bcast = DynamicQuasarMessage::new("Req");
    bcast.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Uint32(30_000_142)]));
    assert!(handlers["eve_public.freelance.project.api.GetAllBroadcastedRequest"](&worker_ctx, &any, &bcast).is_ok());

    assert_eq!(handlers["eve_public.freelance.project.api.GetAllCommittedRequest"](&unauth, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve_public.freelance.project.api.GetAllUnredeemedRequest"](&unauth, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(!handlers["eve_public.freelance.project.api.GetAllUnredeemedRequest"](&worker_ctx, &any, &DynamicQuasarMessage::new("E")).unwrap().value.is_empty());

    let mut stats_req = DynamicQuasarMessage::new("Req");
    stats_req.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Bytes(proj_uuid.clone())]));
    assert!(!handlers["eve_public.freelance.project.api.GetStatsRequest"](&worker_ctx, &any, &stats_req).unwrap().value.is_empty());

    for ep in [
        "eve_public.freelance.project.api.CommitRequest",
        "eve_public.freelance.project.api.ResignRequest",
        "eve_public.freelance.project.api.RedeemRewardRequest",
    ] {
        assert_eq!(handlers[ep](&worker_ctx, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
        assert_eq!(handlers[ep](&unauth, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
    }
    let goal_uuid = corp_projects::corp_goal_uuid(501);
    assert_eq!(handlers["eve_public.freelance.project.api.RedeemRewardRequest"](&worker_ctx, &any, &uuid_req(1, &goal_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert!(handlers["eve_public.freelance.project.api.RedeemRewardRequest"](&worker_ctx, &any, &uuid_req(1, &proj_uuid)).is_ok());

    for ep in [
        "eve_public.freelance.project.api.KickRequest",
        "eve_public.freelance.project.api.CloseRequest",
        "eve_public.freelance.project.api.DeleteRequest",
        "eve.freelance.project.api.admin.SetNameRequest",
        "eve.freelance.project.api.admin.SetDescriptionRequest",
    ] {
        assert_eq!(handlers[ep](&unauth, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_UNAUTHORIZED);
        assert_eq!(handlers[ep](&dir_ctx, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
    }

    let mut kick_msg = uuid_req(1, &proj_uuid);
    assert_eq!(handlers["eve_public.freelance.project.api.KickRequest"](&dir_ctx, &any, &kick_msg).unwrap_err().0, STATUS_BAD_REQUEST);
    kick_msg.fields.insert(2, DynamicValue::Uint32(worker_cid));
    assert_eq!(handlers["eve_public.freelance.project.api.KickRequest"](&other_corp_dir, &any, &kick_msg).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve_public.freelance.project.api.KickRequest"](&worker_ctx, &any, &kick_msg).unwrap_err().0, STATUS_FORBIDDEN);

    let mut sn = uuid_req(1, &proj_uuid);
    sn.fields.insert(2, DynamicValue::String("   ".into()));
    assert_eq!(handlers["eve.freelance.project.api.admin.SetNameRequest"](&dir_ctx, &any, &sn).unwrap_err().0, STATUS_BAD_REQUEST);
    sn.fields.insert(2, DynamicValue::String("Patrol II".into()));
    assert_eq!(handlers["eve.freelance.project.api.admin.SetNameRequest"](&other_corp_dir, &any, &sn).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve.freelance.project.api.admin.SetNameRequest"](&worker_ctx, &any, &sn).unwrap_err().0, STATUS_FORBIDDEN);

    let mut sd = uuid_req(1, &proj_uuid);
    assert_eq!(handlers["eve.freelance.project.api.admin.SetDescriptionRequest"](&dir_ctx, &any, &sd).unwrap_err().0, STATUS_BAD_REQUEST);
    sd.fields.insert(2, DynamicValue::String("Updated".into()));
    assert_eq!(handlers["eve.freelance.project.api.admin.SetDescriptionRequest"](&other_corp_dir, &any, &sd).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve.freelance.project.api.admin.SetDescriptionRequest"](&worker_ctx, &any, &sd).unwrap_err().0, STATUS_FORBIDDEN);

    assert_eq!(handlers["eve_public.freelance.project.api.CloseRequest"](&pm_no_take, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve_public.freelance.project.api.CloseRequest"](&other_corp_dir, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(handlers["eve_public.freelance.project.api.CloseRequest"](&dir_ctx, &any, &uuid_req(1, &proj_uuid)).is_ok());

    assert_eq!(handlers["eve_public.freelance.project.api.DeleteRequest"](&other_corp_dir, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(handlers["eve_public.freelance.project.api.DeleteRequest"](&worker_ctx, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_FORBIDDEN);
    let del_resp = handlers["eve_public.freelance.project.api.DeleteRequest"](&dir_ctx, &any, &uuid_req(1, &proj_uuid)).unwrap();
    assert!(del_resp.type_url.ends_with("eve_public.freelance.project.api.DeleteResponse"));
    assert_eq!(handlers["eve_public.freelance.project.api.GetRequest"](&worker_ctx, &any, &uuid_req(1, &proj_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
}
