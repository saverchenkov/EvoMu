//! Regression tests for Round-7 adversarial review findings (`R7-P0-01`, `R7-P0-02`, `R7-P1-01`, `R7-P1-02`).

#![forbid(unsafe_code)]

use crate::common::{login_pilot, pin_clock};
use eve_db::{
    corp_roles, CorporationRecord, FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord,
    IskCents,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    AuthoritativeContext, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::fw_services::capture_warzone_system;
use eve_server::services::insurgency_services::step_system_insurgency_tick;
use std::collections::{HashMap, HashSet};

fn make_session(sid: u64, uid: u64, cid: i64, corp: i64, role: u64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.station_id = Some(60_003_760);
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s.corp_role = corp_role;
    s
}

fn q_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: vec![0x71; 16],
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "test".into(),
        application_instance_uuid: vec![0x72; 16],
        authoritative_context: Some(
            AuthoritativeContext::for_session_at(
                sess.user_id.map(|u| u as i32),
                sess.character_id.map(|c| c as u32),
                sess.corporation_id.map(|c| c as u32),
                sess.corp_role,
                "tranquility",
                vec![0x73; 16],
                now,
            )
            .with_service_roles(sess.role),
        ),
        trace_context: None,
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, now)
}

fn encode_goal_create_with_escrow(name: &str, target: u64, escrow_isk: i64, init_prog: u64) -> Vec<u8> {
    let (mut msg, mut curr, mut isk, mut pool) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, name, &mut msg);
    encode_varint_field(3, target, &mut msg);
    if init_prog > 0 {
        encode_varint_field(5, init_prog, &mut msg);
    }
    if escrow_isk > 0 {
        encode_varint_field(1, encode_zigzag64(escrow_isk), &mut curr);
        encode_bytes_field(1, &curr, &mut isk);
        encode_bytes_field(2, &isk, &mut pool);
        encode_bytes_field(8, &pool, &mut msg);
    }
    msg
}

#[tokio::test]
async fn test_r7_p0_01_corp_roles_never_escalate_to_quasar_gm_or_bypass_escrow_auth() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let now = ctx.now_unix();
    let (ceo_id, member_id, other_ceo_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64);
    let (corp_a, corp_b) = (98_701_001_i64, 98_701_002_i64);
    let colliding = corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_STARBASE_CONFIG | corp_roles::ROLE_RENT_OFFICE;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&ceo_id) {
            c.corporation_id = corp_a;
        }
        let mut c2 = snap.characters[&ceo_id].clone();
        c2.character_id = member_id;
        c2.user_id = 2;
        c2.corporation_id = corp_a;
        snap.characters.insert(member_id, c2.clone());
        c2.character_id = other_ceo_id;
        c2.user_id = 3;
        c2.corporation_id = corp_b;
        snap.characters.insert(other_ceo_id, c2);

        let mut ca = CorporationRecord {
            corporation_id: corp_a,
            name: "Corp A".into(),
            ticker: "CRPA".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(10_000_000),
            members: HashSet::from([ceo_id, member_id]),
            member_roles: HashMap::from([(ceo_id, u64::MAX), (member_id, colliding)]),
            ..Default::default()
        };
        ca.ensure_wallet_divisions();
        ca.set_division_balance(1, IskCents::from_isk(10_000_000));
        snap.corporations.insert(corp_a, ca);

        let mut cb = CorporationRecord {
            corporation_id: corp_b,
            name: "Corp B".into(),
            ticker: "CRPB".into(),
            ceo_id: other_ceo_id,
            members: HashSet::from([other_ceo_id]),
            member_roles: HashMap::from([(other_ceo_id, u64::MAX)]),
            ..Default::default()
        };
        cb.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, cb);
    });

    let sess_ceo_b = make_session(70103, 3, other_ceo_id, corp_b, 0, u64::MAX);
    let create_b = q_call(&ctx, &sess_ceo_b, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Corp B Goal", 10, 0, 0), now);
    assert_eq!(create_b.status_code, STATUS_OK);
    let goal_b_id_msg = create_b.payload.unwrap().value;

    // 1. Player-corp CEO (`corp_role = u64::MAX`, `session.role = 0`) is rejected (`403`) on GM endpoints & cross-corp DeleteRequest
    let sess_ceo_a = make_session(70101, 1, ceo_id, corp_a, 0, u64::MAX);
    let (mut skin_hex, mut skin_u, mut unit_msg, mut asset_create) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "r7_skin_01", &mut skin_hex);
    encode_bytes_field(1, &skin_hex, &mut skin_u);
    encode_bytes_field(10, &skin_u, &mut unit_msg);
    encode_bytes_field(4, &unit_msg, &mut asset_create);
    encode_varint_field(6, 2, &mut asset_create);
    let (mut t_msg, mut stack_m, mut stacks_m, mut spawn_req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 34, &mut t_msg);
    encode_bytes_field(1, &t_msg, &mut stack_m);
    encode_varint_field(2, 100, &mut stack_m);
    encode_bytes_field(1, &stack_m, &mut stacks_m);
    encode_string_field(2, "Mint Attempt", &mut spawn_req);
    encode_bytes_field(6, &stacks_m, &mut spawn_req);
    assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.asset.api.CreateRequest", asset_create, now).status_code, STATUS_FORBIDDEN);
    assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now).status_code, STATUS_FORBIDDEN);
    assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.entitlement.api.DirectDeliveryRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(q_call(&ctx, &sess_ceo_a, "eve_public.goal.api.DeleteRequest", goal_b_id_msg, now).status_code, STATUS_FORBIDDEN);
    assert_eq!(
        q_call(&ctx, &sess_ceo_a, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Seed", 10, 0, 5), now).status_code,
        STATUS_FORBIDDEN
    );

    // 2. Member holding colliding corp bits (`ROLE_CONTRACT_MANAGER | ROLE_STARBASE_CONFIG | ROLE_RENT_OFFICE`) cannot create goals or bypass escrow
    let sess_mem = make_session(70102, 2, member_id, corp_a, 0, colliding);
    let escrow_req = encode_goal_create_with_escrow("Escrow Drain", 10, 500_000, 0);
    assert_eq!(q_call(&ctx, &sess_mem, "eve_public.goal.api.CreateRequest", escrow_req.clone(), now).status_code, STATUS_FORBIDDEN);

    let pm_colliding = colliding | corp_roles::ROLE_PROJECT_MANAGER;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_a).unwrap().member_roles.insert(member_id, pm_colliding);
    });
    let sess_pm = make_session(70104, 2, member_id, corp_a, 0, pm_colliding);
    assert_eq!(q_call(&ctx, &sess_pm, "eve_public.goal.api.CreateRequest", escrow_req, now).status_code, STATUS_FORBIDDEN);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_a].division_balance(1)), IskCents::from_isk(10_000_000));
}

#[tokio::test]
async fn test_r7_p0_02_title_bitmask_shift_24h_role_delay_edit_project_and_goal_lifecycle() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let now = ctx.now_unix();
    let (ceo_id, pilot_id) = (90_000_001_i64, 90_000_002_i64);
    let corp_id = 98_702_001_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&ceo_id) {
            c.corporation_id = corp_id;
        }
        let mut p = snap.characters[&ceo_id].clone();
        p.character_id = pilot_id;
        p.user_id = 2;
        p.corporation_id = corp_id;
        snap.characters.insert(pilot_id, p);

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Title Corp".into(),
            ticker: "TTLC".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(15_000_000),
            members: HashSet::from([ceo_id, pilot_id]),
            member_roles: HashMap::from([(ceo_id, u64::MAX), (pilot_id, 0)]),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(15_000_000));
        corp.set_division_balance(2, IskCents::from_isk(3_000_000));
        corp.set_division_balance(3, IskCents::from_isk(4_000_000));
        snap.corporations.insert(corp_id, corp);
    });

    let mut sess_ceo = make_session(70201, 1, ceo_id, corp_id, 0, u64::MAX);
    for (tid, name, rmask) in [
        (1_i64, "Recruit", 0_u64),
        (2_i64, "Div2 Bursar", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]),
        (3_i64, "Treasurer PM", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]),
        (4_i64, "Div3 Bursar", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]),
    ] {
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(tid), PyRep::String(name.into()), PyRep::Integer(rmask as i64)], &kw, None)
            .await
            .expect("UpdateTitle");
    }
    let upd_member_15 = |roles: u64, tmask: u64| {
        vec![
            PyRep::Integer(pilot_id), PyRep::String("Member".into()), PyRep::Integer(0), PyRep::Integer(0),
            PyRep::Integer(roles as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
            PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
            PyRep::Integer(60_003_760), PyRep::Integer(tmask as i64), PyRep::Integer(0),
        ]
    };
    ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1), &kw, None).await.expect("Assign Title 1");

    let init_args = [
        PyRep::String("Initial Project".into()),
        PyRep::Integer(10),
        PyRep::Real(1_000.0),
        PyRep::String("DestroyNPCShips".into()),
        PyRep::Bool(false),
        PyRep::Integer(1),
    ];
    let created = ctx.dispatch_rpc(&mut sess_ceo, "corporationSvc", None, "CreateGoalSubmit", &init_args, &kw, None).await.unwrap();
    let pid = match created {
        PyRep::KeyVal(m) => m["projectID"].as_i64().unwrap(),
        other => panic!("unexpected CreateGoalSubmit reply: {other:?}"),
    };

    // 1. Pilot with Title 1 (`tmask = 1`) + stale `session.corp_role = ROLE_DIRECTOR` does NOT inherit Title 3 or stale session role
    let mut sess_p = make_session(70202, 2, pilot_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    let goal_args = [
        PyRep::String("Title1 Exploit".into()),
        PyRep::Integer(5),
        PyRep::Real(10_000.0),
        PyRep::String("DestroyNPCShips".into()),
        PyRep::Bool(false),
        PyRep::Integer(1),
    ];
    assert_eq!(ctx.dispatch_rpc(&mut sess_p, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(
        eve_server::services::social_corp_fleet::social_corp_projects::handle_edit_project(
            &ctx,
            &mut sess_p,
            &[PyRep::Integer(pid), PyRep::String("Hijacked".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let q_escrow = encode_goal_create_with_escrow("Title1 Quasar", 5, 50_000, 0);
    assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), now).status_code, STATUS_FORBIDDEN);

    // 2. Grant Title 2 via live 15-arg `UpdateMember(roles=0, titleMask=1|(1<<1))`: `EditProject` works immediately, Div 2 escrow delayed 24h
    ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1 | (1 << 1)), &kw, None).await.unwrap();
    sess_p.corp_role = 0;
    eve_server::services::social_corp_fleet::social_corp_projects::handle_edit_project(
        &ctx,
        &mut sess_p,
        &[PyRep::Integer(pid), PyRep::String("PM Edited".into())],
    )
    .unwrap()
    .expect("Title 2 PM should edit project immediately");
    ctx.advance_secs(11);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    // Mature Title 2 (+86,401s), then grant Title 3 via 15-arg `UpdateMember(roles=0, titleMask=1|(1<<1)|(1<<2))` AND direct `ROLE_ACCOUNT_CAN_TAKE[2]` via `UpdateMembers`:
    // - `roles=0` must NOT clobber Title 3's 24h delay (Div 1 rejected at t+0s)
    // - Matured Title 2 must NOT be stripped by Title 3's pending delay (Div 2 succeeds at t+0s)
    // - Direct `ROLE_ACCOUNT_CAN_TAKE[2]` via `UpdateMembers` is ALSO delayed 24h (Div 3 rejected at t+0s)
    ctx.advance_secs(86_401);
    ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1 | (1 << 1) | (1 << 2)), &kw, None).await.unwrap();
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1000)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    let t1 = ctx.now_unix();
    assert_eq!(ctx.dispatch_rpc(&mut sess_p, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), t1).status_code, STATUS_FORBIDDEN);

    ctx.advance_secs(11);
    ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap();
    let t1b = ctx.now_unix();
    let div2_ok = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), t1b);
    assert_eq!(div2_ok.status_code, STATUS_OK, "already-matured Title 2 must stay active while Title 3 matures");
    assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", div2_ok.payload.unwrap().value, t1b).status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(2)), IskCents::from_isk(3_000_000));

    // Batch `UpdateMembers`: grant Title 4 (`1 << 3`, Div 3) alongside `new_roles_opt = ROLE_PROJECT_MANAGER` — must NOT clobber Title 4's 24h delay
    let bulk_title_row = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(pilot_id),
        PyRep::Integer(corp_roles::ROLE_PROJECT_MANAGER as i64),
        PyRep::Integer((1 | (1 << 1) | (1 << 2) | (1 << 3)) as i64),
    ])]);
    ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMembers", &[bulk_title_row], &kw, None).await.unwrap();
    ctx.advance_secs(11);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    // Also grant direct `ROLE_ACCOUNT_CAN_TAKE[2]` via both batch `UpdateMembers` and single `UpdateMember` — still delayed 24h
    let bulk_row = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(pilot_id),
        PyRep::Integer((corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]) as i64),
        PyRep::Integer((1 | (1 << 1) | (1 << 2)) as i64),
    ])]);
    ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMembers", &[bulk_row], &kw, None).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_ceo,
        "corpRegistry",
        None,
        "UpdateMember",
        &upd_member_15(corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2], 1 | (1 << 1) | (1 << 2)),
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(11);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    // 3. Mature Title 3 + direct Div 3 (+86,401s); verify promotion does NOT bake Title 3 into `member_roles` upon title removal
    ctx.advance_secs(86_401);
    let now_matured = ctx.now_unix();
    ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1000)], &kw, None).await.unwrap();
    let div1_ok = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), now_matured);
    assert_eq!(div1_ok.status_code, STATUS_OK, "{}", div1_ok.status_message);
    assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", div1_ok.payload.unwrap().value, now_matured).status_code, STATUS_OK);

    // Revoke Title 3 via `UpdateMember`: Div 1 escrow permission must disappear immediately (not baked into `member_roles`)
    ctx.dispatch_rpc(
        &mut sess_ceo,
        "corpRegistry",
        None,
        "UpdateMember",
        &upd_member_15(corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2], 1 | (1 << 1)),
        &kw,
        None,
    )
    .await
    .unwrap();
    assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow, now_matured).status_code, STATUS_FORBIDDEN);

    // 4. Division 3 escrow create + CloseRequest/DeleteRequest refund conservation (`R8-P1-02`)
    ctx.advance_secs(11);
    let now_div3 = ctx.now_unix();
    ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap();
    let g1 = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Div3 Close Goal", 10, 600_000, 0), now_div3);
    let g2 = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Div3 Delete Goal", 10, 900_000, 0), now_div3);
    assert_eq!(g1.status_code, STATUS_OK);
    assert_eq!(g2.status_code, STATUS_OK);
    let (g1_wire, g2_wire) = (g1.payload.unwrap().value, g2.payload.unwrap().value);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(2_500_000));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));

    let close_rsp = q_call(&ctx, &sess_p, "eve_public.goal.api.CloseRequest", g1_wire.clone(), now_matured);
    assert_eq!(close_rsp.status_code, STATUS_OK);
    assert_eq!(close_rsp.payload.unwrap().type_url, "type.googleapis.com/eve_public.goal.api.CloseResponse");
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(3_100_000));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));

    let del_closed = q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", g1_wire, now_matured);
    assert_eq!(del_closed.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(3_100_000));

    let del_rsp = q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", g2_wire, now_matured);
    assert_eq!(del_rsp.status_code, STATUS_OK);
    assert_eq!(del_rsp.payload.unwrap().type_url, "type.googleapis.com/eve_public.goal.api.DeleteResponse");
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(4_000_000));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));
}

#[tokio::test]
async fn test_r7_p1_01_fw_complex_despawn_no_oscillation_solar_system_vp_pct_and_defender_advantage() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let kw = HashMap::new();
    let fw_sys = 30_002_537_u32;
    let (atk_cid, def_cid) = (90_000_001_i64, 90_000_731_i64);
    let (atk_ship, def_ship) = (880_731_001_i64, 880_731_002_i64);

    let mut s_atk = SessionState::new(70301);
    login_pilot(&ctx, &mut s_atk, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut s_atk, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(atk_cid)], &kw, None).await.unwrap();

    let uid_def = ctx.db.provision_player_account("r7_fw_def", "secret");
    ctx.db.with_mut_snapshot(|snap| {
        let mut d = snap.characters[&atk_cid].clone();
        d.character_id = def_cid;
        d.user_id = uid_def;
        snap.characters.insert(def_cid, d);
    });
    let mut s_def = SessionState::new(70302);
    login_pilot(&ctx, &mut s_def, "r7_fw_def", "secret").await;
    ctx.dispatch_rpc(&mut s_def, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(def_cid)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, fid) in [(atk_cid, atk_ship, 500_004_i64), (def_cid, def_ship, 500_001_i64)] {
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.solar_system_id = fw_sys;
                c.station_id = None;
                c.structure_id = None;
                c.active_ship_id = sid;
                c.war_faction_id = Some(fid);
            }
        }
        snap.fw_system_occupations.insert(
            fw_sys,
            FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_runtime.system_advantage.insert(fw_sys, (80.0, 0.0));
        let st = snap.system_states.entry(fw_sys).or_default();
        st.solar_system_id = fw_sys;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600;
        st.fw_capture_seconds_elapsed = 300;
        st.fw_contested_pct = 0.0;
        st.last_fw_capture_tick = 1;
    });

    let fw_h = ctx.systems.get_or_spawn(fw_sys);
    let mk_ball = |id: i64, x: f64| eve_destiny::Ball {
        id,
        type_id: 587,
        position: glam::DVec3::new(x, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };

    // 1. Defender (`500_001`, +80 advantage) de-plexes 14s in 10 ticks (`1.40x`); Attacker (`500_004`) advances 6s in 10 ticks (`0.60x`)
    fw_h.spawn_ball(mk_ball(atk_ship, 80_000.0)).await.unwrap();
    fw_h.spawn_ball(mk_ball(def_ship, 1_000.0)).await.unwrap();
    let base_tick = fw_h.step_manual(1.0).await.unwrap().tick;
    ctx.db.with_mut_snapshot(|s| {
        s.system_states.get_mut(&fw_sys).unwrap().last_fw_capture_tick = base_tick;
    });
    for _ in 0..10 {
        let _ = fw_h.step_manual(1.0).await.unwrap();
    }
    step_system_insurgency_tick(&ctx, fw_sys).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), 286);

    let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
    let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
    for _ in 0..10 {
        let _ = fw_h.step_manual(1.0).await.unwrap();
    }
    step_system_insurgency_tick(&ctx, fw_sys).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), 292);

    // 2. Completing complex despawns it, sets solar-system `fw_contested_pct` to `(20/3000)*100`, and prevents oscillation re-awards
    ctx.db.with_mut_snapshot(|s| {
        s.system_states.get_mut(&fw_sys).unwrap().fw_capture_seconds_elapsed = 599;
    });
    for _ in 0..5 {
        let _ = fw_h.step_manual(1.0).await.unwrap();
    }
    step_system_insurgency_tick(&ctx, fw_sys).await;

    let (lp_once, vp_once, pct_once, pos_after) = ctx.db.with_snapshot(|s| {
        let lp: i64 = s.characters[&atk_cid].loyalty_points.values().copied().sum();
        let vp = s.fw_runtime.system_victory_points.get(&fw_sys).copied().unwrap_or(0);
        let st = &s.system_states[&fw_sys];
        (lp, vp, st.fw_contested_pct, st.fw_complex_position)
    });
    assert!(lp_once > 0);
    assert_eq!(vp_once, 20);
    assert!(pos_after.is_none());
    assert!((pct_once - (20.0 / 3_000.0) * 100.0).abs() < 1e-4);
    assert_eq!(capture_warzone_system(&ctx, fw_sys, 500_004).unwrap_err().code, "SystemNotVulnerable");

    let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
    let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = fw_h.step_manual(1.0).await.unwrap();
    step_system_insurgency_tick(&ctx, fw_sys).await;

    let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
    let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = fw_h.step_manual(1.0).await.unwrap();
    step_system_insurgency_tick(&ctx, fw_sys).await;

    ctx.db.with_snapshot(|s| {
        let lp_after: i64 = s.characters[&atk_cid].loyalty_points.values().copied().sum();
        assert_eq!(lp_after, lp_once);
        assert_eq!(s.fw_runtime.system_victory_points.get(&fw_sys).copied().unwrap_or(0), vp_once);
    });
}

#[tokio::test]
async fn test_r7_p1_02_entitlement_hangar_volume_capacity_enforced_on_delivery_and_redeem() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let now = ctx.now_unix();
    let (holder_id, corp_id, container_loc_id) = (90_000_001_i64, 1_000_009_i64, 770_704_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_loc_id,
            InventoryItemRecord {
                item_id: container_loc_id,
                type_id: 23,
                owner_id: holder_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&holder_id) {
            ch.station_id = Some(container_loc_id);
        }
    });
    let cap_m3 = ctx.sde.types[&23].capacity;
    let unit_trit_qty = ((cap_m3 * 0.6) / 0.01).ceil() as u64;

    let gm_sess = make_session(70401, 1, holder_id, corp_id, service_roles::ROLE_GMH, 0);
    let user_sess = make_session(70402, 1, holder_id, corp_id, 0, 0);

    let (mut t_msg, mut stack_m, mut stacks_m, mut spawn_payload) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 34, &mut t_msg);
    encode_bytes_field(1, &t_msg, &mut stack_m);
    encode_varint_field(2, unit_trit_qty, &mut stack_m);
    encode_bytes_field(1, &stack_m, &mut stacks_m);
    encode_string_field(2, "R7 Volume Asset", &mut spawn_payload);
    encode_bytes_field(6, &stacks_m, &mut spawn_payload);
    assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.asset.api.SpawnRequest", spawn_payload, now).status_code, STATUS_OK);

    let mut asset_uuid = [0u8; 16];
    asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
    asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());
    let (mut ch_m, mut h_m, mut a_m, mut g_m, mut ent_id) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, holder_id as u64, &mut ch_m);
    encode_bytes_field(1, &ch_m, &mut h_m);
    encode_bytes_field(1, &asset_uuid, &mut a_m);
    encode_string_field(1, "r7-vol", &mut g_m);
    encode_bytes_field(1, &h_m, &mut ent_id);
    encode_bytes_field(2, &a_m, &mut ent_id);
    encode_bytes_field(3, &g_m, &mut ent_id);

    let mk_dd = |qty: u64| {
        let (mut entry, mut dd) = (Vec::new(), Vec::new());
        encode_bytes_field(2, &ent_id, &mut entry);
        encode_varint_field(10, qty, &mut entry);
        encode_bytes_field(2, &entry, &mut dd);
        dd
    };

    assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(2), now).status_code, STATUS_BAD_REQUEST);
    assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(1), now).status_code, STATUS_OK);
    assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(1), now).status_code, STATUS_BAD_REQUEST);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_704_002,
            InventoryItemRecord {
                item_id: 770_704_002,
                type_id: 34,
                owner_id: holder_id,
                location_id: container_loc_id,
                flag: InventoryFlag::Hangar,
                quantity: unit_trit_qty as i32,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut redeem_wire = Vec::new();
    encode_bytes_field(1, &ent_id, &mut redeem_wire);
    assert_eq!(q_call(&ctx, &user_sess, "eve_public.assetholding.entitlement.api.RedeemRequest", redeem_wire.clone(), now).status_code, STATUS_BAD_REQUEST);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&770_704_002);
    });
    assert_eq!(q_call(&ctx, &user_sess, "eve_public.assetholding.entitlement.api.RedeemRequest", redeem_wire, now).status_code, STATUS_OK);
}
