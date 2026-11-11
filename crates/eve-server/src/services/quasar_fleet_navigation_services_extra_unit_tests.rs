//! Extra unit and mutation-killing tests for `services::quasar_fleet_navigation_services`.

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dispatch(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    req: &GoogleAny,
) -> Result<GoogleAny, (u32, String)> {
    let norm = req.normalized_type_name();
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(norm)
        .cloned()
        .expect("registered handler");
    let proto_name = norm.replace(".wallet.assetholding.", ".assetholding.");
    let dyn_msg = router
        .descriptors
        .decode_dynamic_or_wire(&proto_name, &req.value)
        .unwrap_or_else(|_| {
            crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
        });
    handler(ctx, req, &dyn_msg)
}

fn dispatch_dyn(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    type_name: &str,
    dyn_msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(type_name)
        .cloned()
        .expect("registered handler");
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: Vec::new(),
    };
    handler(ctx, &any, dyn_msg)
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db)
}

fn seed_char(db: &WorldRepository, char_id: i64, corp_id: i64, isk: i64, lp: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        rec.corporation_id = corp_id;
        rec.balance = IskCents::from_isk(isk);
        rec.loyalty_points.clear();
        rec.loyalty_points.insert(1_000_419, lp);
        s.characters.insert(char_id, rec);
    });
}

fn make_ctx(char_id: Option<u32>, role: u64, now_unix: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        now_unix,
    )
    .with_service_roles(role)
}

#[test]
fn admin_daily_goal_handlers_partial_progress_unknown_uuid_and_explicit_target_char() {
    let (router, db) = make_router_and_db();
    let cid = 211200401u32;
    let other_cid = 211200402u32;
    seed_char(&db, cid as i64, 98000401, 1_000_000, 1_000);
    seed_char(&db, other_cid as i64, 98000401, 1_000_000, 1_000);

    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 300_000);
    let admin_no_char = make_ctx(None, ROLE_PROGRAMMER, 300_000);

    let goals = router
        .progression
        .daily_goals
        .get_or_init_goals_at(other_cid, 300_000);
    let g1_uuid = goals[0].uuid; // target 25

    // 1. ProgressRequest with unknown UUID -> STATUS_NOT_FOUND
    let mut unk_id = DynamicQuasarMessage::new("id");
    unk_id.set(1, DynamicValue::Bytes(vec![0xDE; 16]));
    let mut unk_prog = DynamicQuasarMessage::new("req");
    unk_prog.set(1, DynamicValue::Message(Box::new(unk_id)));
    unk_prog.set(3, DynamicValue::Uint64(5));
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.dailygoal.api.admin.ProgressRequest",
            &unk_prog
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 2. ProgressRequest with explicit target_cid (field 2) and partial delta (5 < 25) using Int32/Int64
    let mut ok_id = DynamicQuasarMessage::new("id");
    ok_id.set(1, DynamicValue::Bytes(g1_uuid.to_vec()));
    let mut char_sub = DynamicQuasarMessage::new("char");
    char_sub.set(1, DynamicValue::Int32(other_cid as i32));
    let mut participant_progress = DynamicQuasarMessage::new("req");
    participant_progress.set(1, DynamicValue::Message(Box::new(ok_id.clone())));
    participant_progress.set(2, DynamicValue::Message(Box::new(char_sub.clone())));
    participant_progress.set(3, DynamicValue::Int64(5));
    dispatch_dyn(
        &router,
        &admin_no_char,
        "eve.dailygoal.api.admin.ProgressRequest",
        &participant_progress,
    )
    .expect("partial progress for explicit target_cid");
    let g1_after = router
        .progression
        .daily_goals
        .get_or_init_goals_at(other_cid, 300_000)
        .into_iter()
        .find(|g| g.uuid == g1_uuid)
        .unwrap();
    assert_eq!(g1_after.current_progress, 5);
    assert!(!g1_after.completed);

    // 3. ExpireRequest validation: missing uuid -> 400, missing char on admin_no_char -> 400, explicit target_cid -> 200
    let empty_d = DynamicQuasarMessage::new("empty");
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.dailygoal.api.admin.ExpireRequest",
            &empty_d
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut exp_no_char = DynamicQuasarMessage::new("req");
    exp_no_char.set(1, DynamicValue::Message(Box::new(ok_id.clone())));
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin_no_char,
            "eve.dailygoal.api.admin.ExpireRequest",
            &exp_no_char
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut exp_explicit = exp_no_char;
    exp_explicit.set(2, DynamicValue::Message(Box::new(char_sub)));
    dispatch_dyn(
        &router,
        &admin_no_char,
        "eve.dailygoal.api.admin.ExpireRequest",
        &exp_explicit,
    )
    .expect("ExpireRequest with explicit target_cid");

    // 4. AutoRedeemRequest with admin_no_char (no effective_character_id) -> succeeds cleanly
    dispatch_dyn(
        &router,
        &admin_no_char,
        "eve.dailygoal.api.admin.AutoRedeemRequest",
        &empty_d,
    )
    .expect("AutoRedeemRequest without active char");
}

#[test]
fn lp_operation_parser_edge_cases_npc_corp_and_overflow_guards() {
    let (router, db) = make_router_and_db();
    let cid = 211200403u32;
    let npc_corp = 1_000_009u32; // < 98_000_000 -> does NOT duplicate Paragon EverMarks to corp
    let paragon = crate::services::lp_store::PARAGON_CORP_ID as u32;
    seed_char(&db, cid as i64, npc_corp as i64, 1_000_000, 1_000);

    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 400_000);
    let admin_no_char = make_ctx(None, ROLE_PROGRAMMER, 400_000);

    let make_op_dyn = |uuid: &[u8],
                       char_opt: Option<u32>,
                       corp_opt: Option<u32>,
                       issuer: u32,
                       amt: Option<DynamicValue>,
                       ctx_b: Vec<u8>| {
        let mut id_m = DynamicQuasarMessage::new("id");
        id_m.set(1, DynamicValue::Bytes(uuid.to_vec()));
        let mut w_m = DynamicQuasarMessage::new("wallet");
        if let Some(c) = char_opt {
            let mut cm = DynamicQuasarMessage::new("c");
            cm.set(1, DynamicValue::Uint32(c));
            w_m.set(1, DynamicValue::Message(Box::new(cm)));
        }
        if let Some(co) = corp_opt {
            let mut com = DynamicQuasarMessage::new("co");
            com.set(1, DynamicValue::Uint32(co));
            w_m.set(2, DynamicValue::Message(Box::new(com)));
        }
        if issuer > 0 {
            let mut im = DynamicQuasarMessage::new("iss");
            im.set(1, DynamicValue::Uint32(issuer));
            w_m.set(3, DynamicValue::Message(Box::new(im)));
        }
        let mut attr_m = DynamicQuasarMessage::new("attr");
        attr_m.set(1, DynamicValue::Message(Box::new(w_m)));
        if let Some(a) = amt {
            let mut cur_m = DynamicQuasarMessage::new("cur");
            cur_m.set(1, a);
            attr_m.set(2, DynamicValue::Message(Box::new(cur_m)));
        }
        let mut d = DynamicQuasarMessage::new("op");
        d.set(1, DynamicValue::Message(Box::new(id_m)));
        d.set(2, DynamicValue::Message(Box::new(attr_m)));
        if !ctx_b.is_empty() {
            d.set(3, DynamicValue::Bytes(ctx_b));
        }
        d
    };

    // 1. Missing op_attr (field 2) or missing wallet_id (field 1 of op_attr) -> 400
    let mut d_no_attr = DynamicQuasarMessage::new("op");
    let mut id_m = DynamicQuasarMessage::new("id");
    id_m.set(1, DynamicValue::Bytes(vec![1u8; 16]));
    d_no_attr.set(1, DynamicValue::Message(Box::new(id_m.clone())));
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            &d_no_attr
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let mut d_no_wallet = DynamicQuasarMessage::new("op");
    d_no_wallet.set(1, DynamicValue::Message(Box::new(id_m)));
    d_no_wallet.set(
        2,
        DynamicValue::Message(Box::new(DynamicQuasarMessage::new("attr"))),
    );
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            &d_no_wallet
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. character_id == 0, corporation_id == 0, missing owner when default_cid is None, and amount > i64::MAX -> 400
    for bad_op in [
        make_op_dyn(&[2; 16], Some(0), None, paragon, Some(DynamicValue::Uint64(10)), Vec::new()),
        make_op_dyn(&[3; 16], None, Some(0), paragon, Some(DynamicValue::Uint64(10)), Vec::new()),
        make_op_dyn(&[4; 16], None, None, paragon, Some(DynamicValue::Uint64(10)), Vec::new()),
        make_op_dyn(
            &[5; 16],
            Some(cid),
            None,
            paragon,
            Some(DynamicValue::Uint64(u64::MAX)),
            Vec::new(),
        ),
    ] {
        assert_eq!(
            dispatch_dyn(
                &router,
                &admin_no_char,
                "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
                &bad_op
            )
            .unwrap_err()
            .0,
            STATUS_BAD_REQUEST
        );
    }

    // 3. Unknown character in DepositRequest and WithdrawRequest -> STATUS_NOT_FOUND
    let ghost_op = make_op_dyn(
        &[6; 16],
        Some(999_999_999),
        None,
        paragon,
        Some(DynamicValue::Uint64(100)),
        Vec::new(),
    );
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            &ghost_op
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
            &ghost_op
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 4. Default owner fallback (no char_id/corp_id in wallet_id -> uses admin's cid) + NPC corp does NOT duplicate Paragon LP
    let def_owner_op = make_op_dyn(
        &[7; 16],
        None,
        None,
        paragon,
        Some(DynamicValue::Int32(250)),
        b"ctx_bytes".to_vec(),
    );
    dispatch_dyn(
        &router,
        &admin,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        &def_owner_op,
    )
    .expect("default owner deposit");
    db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&(cid as i64)].loyalty_points[&(paragon as i64)],
            1_250
        );
        assert!(!s
            .corp_lp_balances
            .contains_key(&(npc_corp as i64, paragon as i64)));
    });

    // 5. Character & Corporation LP balance overflow -> STATUS_BAD_REQUEST
    db.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&(cid as i64))
            .unwrap()
            .loyalty_points
            .insert(paragon as i64, i64::MAX);
        s.corp_lp_balances
            .insert((98000499, paragon as i64), i64::MAX);
    });
    let char_ovf = make_op_dyn(
        &[8; 16],
        Some(cid),
        None,
        paragon,
        Some(DynamicValue::Uint64(1)),
        Vec::new(),
    );
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            &char_ovf
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let corp_ovf = make_op_dyn(
        &[9; 16],
        None,
        Some(98000499),
        paragon,
        Some(DynamicValue::Uint64(1)),
        Vec::new(),
    );
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            &corp_ovf
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn bounty_multiplier_zero_sys_id_default_pct_and_exact_boundaries() {
    let (router, db) = make_router_and_db();
    let admin = make_ctx(Some(211200410), ROLE_PROGRAMMER, 500_000);
    let rpc = "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest";

    // 1. sys_id == 0 -> STATUS_BAD_REQUEST
    let mut sys0 = DynamicQuasarMessage::new("sys");
    sys0.set(1, DynamicValue::Uint32(0));
    let mut d0 = DynamicQuasarMessage::new("req");
    d0.set(1, DynamicValue::Message(Box::new(sys0)));
    assert_eq!(
        dispatch_dyn(&router, &admin, rpc, &d0).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // 2. Default input_pct when field 2 and field 3 are omitted -> 100% (1.0x), and existing bank balances are encoded
    let sys_id = 30000142u32;
    db.with_mut_snapshot(|s| {
        s.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                dbs_multiplier: 1.2,
                ess_main_bank: IskCents(55),       // 0 units, 550_000_000 nanos
                ess_reserve_bank: IskCents(-250),  // negative cents zigzag test
                ..Default::default()
            },
        );
    });
    let mut sys_ok = DynamicQuasarMessage::new("sys");
    sys_ok.set(1, DynamicValue::Uint64(sys_id as u64));
    let mut d_def = DynamicQuasarMessage::new("req");
    d_def.set(1, DynamicValue::Message(Box::new(sys_ok.clone())));
    dispatch_dyn(&router, &admin, rpc, &d_def).expect("default 100%");
    let mult_def = db.with_snapshot(|s| s.system_states[&sys_id].dbs_multiplier);
    assert!((mult_def - 1.0).abs() < 1e-9);

    // 3. Exact boundaries: 0.0 (units=0, nanos=0) -> OK; 10.0 (units=10, nanos=0) -> OK; 10.000001 -> BAD_REQUEST
    let mut det_zero = DynamicQuasarMessage::new("det");
    det_zero.set(1, DynamicValue::Int64(0));
    det_zero.set(2, DynamicValue::Int32(0));
    let mut d_zero = DynamicQuasarMessage::new("req");
    d_zero.set(1, DynamicValue::Message(Box::new(sys_ok.clone())));
    d_zero.set(3, DynamicValue::Message(Box::new(det_zero)));
    dispatch_dyn(&router, &admin, rpc, &d_zero).expect("0.0 boundary");
    assert_eq!(db.with_snapshot(|s| s.system_states[&sys_id].dbs_multiplier), 0.0);

    let mut det_ten = DynamicQuasarMessage::new("det");
    det_ten.set(1, DynamicValue::Uint32(10));
    det_ten.set(2, DynamicValue::Uint32(0));
    let mut d_ten = DynamicQuasarMessage::new("req");
    d_ten.set(1, DynamicValue::Message(Box::new(sys_ok.clone())));
    d_ten.set(3, DynamicValue::Message(Box::new(det_ten)));
    dispatch_dyn(&router, &admin, rpc, &d_ten).expect("10.0 boundary");
    assert_eq!(db.with_snapshot(|s| s.system_states[&sys_id].dbs_multiplier), 10.0);

    let mut det_over = DynamicQuasarMessage::new("det");
    det_over.set(1, DynamicValue::Uint32(10));
    det_over.set(2, DynamicValue::Uint32(1_000));
    let mut d_over = DynamicQuasarMessage::new("req");
    d_over.set(1, DynamicValue::Message(Box::new(sys_ok)));
    d_over.set(3, DynamicValue::Message(Box::new(det_over)));
    assert_eq!(
        dispatch_dyn(&router, &admin, rpc, &d_over).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let mut wire_req = Vec::new();
    let mut s_msg = Vec::new();
    encode_varint_field(1, sys_id as u64, &mut s_msg);
    encode_bytes_field(1, &s_msg, &mut wire_req);
    encode_varint_field(2, 100, &mut wire_req);
    assert!(dispatch(
        &router,
        &admin,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value: wire_req,
        }
    )
    .is_ok());
}

#[test]
fn register_handlers_and_lp_deposit_withdraw_idempotent_notices_kill_mutants() {
    use crate::services::quasar_fleet_navigation_services::register_handlers;
    use crate::services::quasar_social_chat::drain_local_notices;

    let (router, db) = make_router_and_db();
    register_handlers(
        &router,
        Arc::clone(&db),
        shared_sde(),
        Arc::clone(&router.progression.daily_goals),
    );
    let cid = 211_200_499_u32;
    let paragon = crate::services::lp_store::PARAGON_CORP_ID as u32;
    seed_char(&db, cid as i64, 98_000_499, 1_000_000, 1_000);
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 500_000);

    let make_lp_op = |uuid: &[u8; 16], amount: u64| {
        let mut id_m = DynamicQuasarMessage::new("id");
        id_m.set(1, DynamicValue::Bytes(uuid.to_vec()));
        let mut cm = DynamicQuasarMessage::new("c");
        cm.set(1, DynamicValue::Uint32(cid));
        let mut im = DynamicQuasarMessage::new("iss");
        im.set(1, DynamicValue::Uint32(paragon));
        let mut wm = DynamicQuasarMessage::new("wallet");
        wm.set(1, DynamicValue::Message(Box::new(cm)));
        wm.set(3, DynamicValue::Message(Box::new(im)));
        let mut cur_m = DynamicQuasarMessage::new("cur");
        cur_m.set(1, DynamicValue::Uint64(amount));
        let mut attr_m = DynamicQuasarMessage::new("attr");
        attr_m.set(1, DynamicValue::Message(Box::new(wm)));
        attr_m.set(2, DynamicValue::Message(Box::new(cur_m)));
        let mut req = DynamicQuasarMessage::new("op");
        req.set(1, DynamicValue::Message(Box::new(id_m)));
        req.set(2, DynamicValue::Message(Box::new(attr_m)));
        req
    };

    let _ = drain_local_notices();
    let dep_uuid = [0x41; 16];
    let dep_req = make_lp_op(&dep_uuid, 250);
    dispatch_dyn(
        &router,
        &admin,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        &dep_req,
    )
    .expect("first DepositRequest succeeds");
    let dep_notices = drain_local_notices();
    assert!(
        dep_notices.iter().any(|(_, any, _)| any.type_url.ends_with("BalanceAdded")),
        "first deposit (!already_done) must queue BalanceAdded notice"
    );
    assert_eq!(db.with_snapshot(|s| s.characters[&(cid as i64)].loyalty_points[&(paragon as i64)]), 1_250);

    // Replay with same op_uuid: already_done == true -> must NOT emit duplicate BalanceAdded notice
    dispatch_dyn(
        &router,
        &admin,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        &dep_req,
    )
    .expect("idempotent DepositRequest succeeds");
    let dep_replay_notices = drain_local_notices();
    assert!(
        dep_replay_notices.is_empty(),
        "replayed deposit (already_done) must not emit BalanceAdded notice"
    );

    let wd_uuid = [0x42; 16];
    let wd_req = make_lp_op(&wd_uuid, 100);
    dispatch_dyn(
        &router,
        &admin,
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        &wd_req,
    )
    .expect("first WithdrawRequest succeeds");
    let wd_notices = drain_local_notices();
    assert!(
        wd_notices.iter().any(|(_, any, _)| any.type_url.ends_with("BalanceRemoved")),
        "first withdraw (!already_done) must queue BalanceRemoved notice"
    );
    assert_eq!(db.with_snapshot(|s| s.characters[&(cid as i64)].loyalty_points[&(paragon as i64)]), 1_150);

    // Replay withdraw with same op_uuid: already_done == true -> must NOT emit duplicate BalanceRemoved notice
    dispatch_dyn(
        &router,
        &admin,
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        &wd_req,
    )
    .expect("idempotent WithdrawRequest succeeds");
    let wd_replay_notices = drain_local_notices();
    assert!(
        wd_replay_notices.is_empty(),
        "replayed withdraw (already_done) must not emit BalanceRemoved notice"
    );
}
