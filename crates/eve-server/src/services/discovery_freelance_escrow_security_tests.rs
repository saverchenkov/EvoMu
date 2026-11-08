//! Regression security tests for Round-7 Track TD P0B (`#460`, `#462`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN};
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::project_discovery::{handle_project_discovery, MIN_SUBMISSION_INTERVAL_SECS};
use crate::services::quasar_social_freelance::{self, FreelanceProjectRecord, QuasarFreelanceState};
use eve_db::{
    corp_roles, ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue,
    GoogleAny, QuasarDescriptorRegistry,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
const PD_SERVICE_NAME: &str = "ProjectDiscovery";

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn pd_rpc(
    ctx: &crate::router::ServerContext,
    sess: &mut eve_machonet::SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    handle_project_discovery(ctx, sess, PD_SERVICE_NAME, method, args, &HashMap::new()).unwrap()
}

fn uuid_req(uuid: &[u8]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(1, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn r7_td_04_project_discovery_empty_polygons_and_same_second_rate_limit_blocked() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);

    let bal_start = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // Acquire task_1
    let t1 = pd_rpc(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let t1_id = t1.dict_get("task_id").cloned().unwrap();

    // 1. Empty polygon / empty classification variants must be rejected with 0 rewards
    let invalid_payloads = [
        PyRep::keyval([("polygons", PyRep::List(vec![]))]),
        PyRep::keyval([("gates", PyRep::List(vec![]))]),
        PyRep::keyval([("polygons", PyRep::List(vec![PyRep::List(vec![])]))]),
        PyRep::String("{\"polygons\":[]}".into()),
        PyRep::String("{\"gates\":[]}".into()),
        PyRep::String("{}".into()),
        PyRep::List(vec![]),
        PyRep::None,
    ];
    for payload in invalid_payloads {
        let err = pd_rpc(&ctx, &mut sess, "post_classification", &[t1_id.clone(), payload]).unwrap_err();
        assert_eq!(err.code, "InvalidClassification");
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_start);

    // 2. Valid non-empty polygon classification on task_1 succeeds
    let valid_poly = PyRep::keyval([(
        "polygons",
        PyRep::List(vec![PyRep::List(vec![
            PyRep::List(vec![PyRep::Real(0.1), PyRep::Real(0.2)]),
            PyRep::List(vec![PyRep::Real(0.5), PyRep::Real(0.6)]),
            PyRep::List(vec![PyRep::Real(0.2), PyRep::Real(0.8)]),
        ])]),
    )]);
    let res1 = pd_rpc(&ctx, &mut sess, "post_classification", &[t1_id, valid_poly.clone()]).unwrap();
    assert!(res1.dict_get("ISK_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    let bal_after_first = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert!(bal_after_first > bal_start);

    // 3. Same-second submission (now == prev_ts) for task_2 must be rejected cleanly
    let t2 = pd_rpc(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let t2_id = t2.dict_get("task_id").cloned().unwrap();
    let rate_err = pd_rpc(&ctx, &mut sess, "post_classification", &[t2_id.clone(), valid_poly.clone()]).unwrap_err();
    assert_eq!(rate_err.code, "ProjectDiscoverySubmissionTooFast");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_after_first);

    // 4. Advancing >= MIN_SUBMISSION_INTERVAL_SECS allows task_2 to succeed normally
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    let res2 = pd_rpc(&ctx, &mut sess, "post_classification", &[t2_id, valid_poly]).unwrap();
    assert!(res2.dict_get("ISK_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance) > bal_after_first);
}

#[test]
fn r7_td_06_freelance_escrow_payout_wallet_sync_role_and_item_lock_enforced() {
    // 1. Backwards-compatible serde default on `initial_reward_isk`
    let legacy_json = r#"{"uuid":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"owner_corp_id":1000009,"creator_char_id":90000001,"name":"Legacy","description":"","career":1,"deadline":9999999,"created":1,"target_progress":10,"current_progress":0,"state":1,"committed_participants":[],"contributors":{},"reward_isk":500000}"#;
    let parsed_legacy: FreelanceProjectRecord = serde_json::from_str(legacy_json).expect("legacy deserializes");
    assert_eq!(parsed_legacy.initial_reward_isk, None);

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    quasar_social_freelance::register(&router, Arc::clone(&db));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();

    let dir_cid = 90_000_001_u32;
    let worker_cid = 90_000_002_u32;
    let pm_no_take_cid = 90_000_003_u32;
    let corp_id = 1_000_009_u32;
    let station_id = 60_003_760_i64;
    let item_id = 990_001_001_i64;

    db.with_mut_snapshot(|snap| {
        for cid in [dir_cid as i64, worker_cid as i64, pm_no_take_cid as i64] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id as i64;
                ch.station_id = Some(station_id);
            }
        }
        if let Some(ch) = snap.characters.get_mut(&(worker_cid as i64)) {
            ch.balance = IskCents::ZERO;
        }
        let corp = snap.corporations.entry(corp_id as i64).or_insert_with(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = corp_id as i64;
            c
        });
        corp.set_division_balance(1, IskCents::from_isk(5_000_000));
        corp.members
            .extend([dir_cid as i64, worker_cid as i64, pm_no_take_cid as i64]);
        corp.member_roles
            .insert(dir_cid as i64, corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(pm_no_take_cid as i64, corp_roles::ROLE_PROJECT_MANAGER);
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34, // Tritanium
                owner_id: worker_cid as i64,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 30,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
    });

    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(dir_cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let worker_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(worker_cid),
        Some(corp_id),
        0,
        "tranquility",
        vec![1],
    );
    let pm_no_take_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(pm_no_take_cid),
        Some(corp_id),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );

    // Create item-delivery freelance project: target = 30 Tritanium, batch_size = 10, total escrow = 1_000_000 ISK
    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::String("DeliverItem".into()));
    let mut p_type = DynamicQuasarMessage::new("Param");
    let mut p_opt = DynamicQuasarMessage::new("Opt");
    p_opt.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::String("34".into())]));
    p_type.fields.insert(3, DynamicValue::Message(Box::new(p_opt)));
    let mut p_batch = DynamicQuasarMessage::new("Param");
    let mut b_opt = DynamicQuasarMessage::new("Opt");
    b_opt.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::String("10".into())]));
    p_batch.fields.insert(3, DynamicValue::Message(Box::new(b_opt)));
    cfg.fields.insert(
        3,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(p_type)), DynamicValue::Message(Box::new(p_batch))]),
    );

    let mut escrow_msg = DynamicQuasarMessage::new("Escrow");
    escrow_msg.fields.insert(3, DynamicValue::Uint64(1_000_000));

    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Deliver Tritanium".into()));
    create_msg.fields.insert(2, DynamicValue::String("Mineral supply".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(30));
    create_msg.fields.insert(6, DynamicValue::Message(Box::new(cfg)));
    create_msg.fields.insert(11, DynamicValue::Message(Box::new(escrow_msg)));

    handlers["eve_public.freelance.project.api.CreateRequest"](&dir_ctx, &dummy_any, &create_msg)
        .expect("director creates funded project");

    // Verify corp master_wallet and wallet division 1 stay strictly synchronized (4_000_000 ISK remaining)
    db.with_snapshot(|snap| {
        let corp = &snap.corporations[&(corp_id as i64)];
        assert_eq!(corp.master_wallet, IskCents::from_isk(4_000_000));
        assert_eq!(corp.division_balance(1), IskCents::from_isk(4_000_000));
    });

    let (pid, proj_uuid) = db.with_snapshot(|snap| {
        let (&pid, _) = snap.alliance_war_state.project_meta.iter().find(|(_, m)| m.1).unwrap();
        let u = snap.alliance_war_state.project_details[&pid].uuid_bytes.clone();
        (pid, u)
    });
    let state_view = QuasarFreelanceState::from_db(Arc::clone(&db));
    let rec = &state_view.projects.read().unwrap()[&proj_uuid];
    assert_eq!(rec.initial_reward_isk, Some(1_000_000));
    assert_eq!(rec.reward_isk, 1_000_000);

    // Worker commits
    handlers["eve_public.freelance.project.api.CommitRequest"](&worker_ctx, &dummy_any, &uuid_req(&proj_uuid))
        .expect("worker commits");

    // Lock item_id in an active contract escrow -> DeliverRequest must NOT consume locked item
    db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            77_777,
            ContractRecord {
                contract_id: 77_777,
                status: "Outstanding".into(),
                item_ids: vec![item_id],
                ..ContractRecord::default()
            },
        );
    });
    let build_deliver_msg = |qty: u64| -> DynamicQuasarMessage {
        let mut msg = uuid_req(&proj_uuid);
        let mut item_ref = DynamicQuasarMessage::new("ItemRef");
        item_ref.fields.insert(1, DynamicValue::Uint64(item_id as u64));
        msg.fields.insert(3, DynamicValue::Message(Box::new(item_ref)));
        msg.fields.insert(4, DynamicValue::Uint64(qty));
        msg
    };
    let locked_resp = handlers["eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest"](
        &worker_ctx,
        &dummy_any,
        &build_deliver_msg(10),
    )
    .expect("locked deliver returns rejection response");
    assert!(!locked_resp.value.is_empty());
    db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_id].quantity, 30, "Escrow-locked item must never be consumed");
        assert_eq!(snap.corp_projects[&pid].current_quantity, 0);
    });

    // Unlock item and deliver 3 batches of 10 units, redeeming after each batch:
    // Payouts must stay fixed per batch (333_330.00 ISK + 333_330.00 ISK) with final remainder
    // distributed on completion (333_340.00 ISK) = exact 1_000_000.00 ISK (zero decay, zero stranding)
    db.with_mut_snapshot(|snap| {
        snap.contracts.remove(&77_777);
    });
    for expected_cum_cents in [33_333_000_i64, 66_666_000_i64, 100_000_000_i64] {
        handlers["eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest"](
            &worker_ctx,
            &dummy_any,
            &build_deliver_msg(10),
        )
        .expect("unlocked delivery succeeds");
        handlers["eve_public.freelance.project.api.RedeemRewardRequest"](
            &worker_ctx,
            &dummy_any,
            &uuid_req(&proj_uuid),
        )
        .expect("redeem reward succeeds");
        db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&(worker_cid as i64)].balance.raw_cents(), expected_cum_cents);
        });
    }
    db.with_snapshot(|snap| {
        assert_eq!(snap.corp_projects[&pid].escrow_remaining, IskCents::ZERO);
    });

    // Second funded project: Project Manager without ROLE_ACCOUNT_CAN_TAKE[0] cannot close & refund escrow;
    // Director closes and refunds exact escrow into synced master_wallet + division_balance(1)
    handlers["eve_public.freelance.project.api.CreateRequest"](&dir_ctx, &dummy_any, &create_msg)
        .expect("director creates second project");
    let second_uuid = db.with_snapshot(|snap| {
        snap.alliance_war_state
            .project_details
            .iter()
            .find(|(&id, _)| id != pid)
            .map(|(_, d)| d.uuid_bytes.clone())
            .unwrap()
    });
    assert_eq!(
        handlers["eve_public.freelance.project.api.CloseRequest"](
            &pm_no_take_ctx,
            &dummy_any,
            &uuid_req(&second_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    handlers["eve_public.freelance.project.api.CloseRequest"](
        &dir_ctx,
        &dummy_any,
        &uuid_req(&second_uuid),
    )
    .expect("director closes second project");
    db.with_snapshot(|snap| {
        let corp = &snap.corporations[&(corp_id as i64)];
        assert_eq!(corp.master_wallet, IskCents::from_isk(4_000_000));
        assert_eq!(corp.division_balance(1), IskCents::from_isk(4_000_000));
    });
    let _ = (encode_bytes_field, encode_varint_field);
}
