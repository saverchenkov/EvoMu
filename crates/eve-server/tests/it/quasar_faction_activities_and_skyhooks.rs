//! Live `publicGatewaySvc` integration tests for Batch 33 Quasar services.

use eve_db::{
    corp_roles, FwSystemOccupationRecord, PersistedMercenaryDen, PersistedSovHubData,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::PaymentScheduleRecord;
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
};
use std::collections::HashMap;

fn create_user_payment_schedule(
    db: &eve_db::WorldRepository,
    user_id: u64,
    schedule_type: u32,
    offer_id: &str,
    interval_days: u32,
    now_unix: u64,
) -> PaymentScheduleRecord {
    db.with_mut_snapshot(|snap| {
        let mut schedules: Vec<PaymentScheduleRecord> = snap
            .service_state_blobs
            .get("payment_schedules")
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        let next_id = schedules.iter().map(|s| s.id).max().unwrap_or(1000) + 1;
        let rec = PaymentScheduleRecord {
            id: next_id,
            user_id,
            schedule_type,
            offer_id: offer_id.to_string(),
            interval_days,
            next_payment_unix: now_unix + u64::from(interval_days) * 86_400,
            created_unix: now_unix,
        };
        schedules.push(rec.clone());
        if let Ok(json) = serde_json::to_string(&schedules) {
            snap.service_state_blobs.insert("payment_schedules".to_string(), json);
        }
        rec
    })
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64, admin: bool) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    sess.solar_system_id = Some(30_000_142);
    sess.role = if admin {
        eve_machonet::service_roles::ROLE_GM_DEFAULT
    } else {
        eve_machonet::service_roles::ROLE_PLAYER
    };
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"b33-closure-corr".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-b33-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn encode_u64_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_uuid_id(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_ach_uuid_ident(field_no: u32, id: u32, out: &mut Vec<u8>) {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    encode_uuid_id(field_no, &uuid, out);
}

fn decode_dyn(ctx: &ServerContext, rsp: &EvePublicResponse) -> DynamicQuasarMessage {
    let any = rsp.payload.as_ref().expect("response payload");
    ctx.quasar_gateway
        .descriptors
        .decode_dynamic(any.normalized_type_name(), &any.value)
        .expect("decode DynamicQuasarMessage")
}

fn seed_char(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    uid: u64,
    corp_id: i64,
    sys_id: u32,
) {
    let mut ch = snap
        .characters
        .values()
        .next()
        .cloned()
        .expect("at least one seeded character");
    ch.character_id = cid;
    ch.user_id = uid;
    ch.name = format!("Pilot_{cid}");
    ch.corporation_id = corp_id;
    ch.solar_system_id = sys_id;
    ch.station_id = None;
    ch.structure_id = None;
    snap.characters.insert(cid, ch);
}

async fn subscribe_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
) -> tokio::sync::mpsc::Receiver<eve_proto::quasar::EvePublicNotice> {
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx")
}

fn drain_notice_names(
    rx: &mut tokio::sync::mpsc::Receiver<eve_proto::quasar::EvePublicNotice>,
) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p.normalized_type_name().to_string());
        }
    }
    out
}

fn seed_structure(snap: &mut eve_db::WorldStateSnapshot, id: i64, owner: i64, sys: u32, type_id: u32) {
    snap.structures.insert(id, UpwellStructureRecord {
        structure_id: id, type_id, owner_corp_id: owner, solar_system_id: sys,
        name: format!("Structure {id}"), position: [200_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 0,
        shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    });
}

#[tokio::test]
async fn test_b33_faction_activity_admin_and_payment_schedule() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
    let ctx = ServerContext::new();
    let mut admin_sess = make_session(1, 10, 90_000_010, 1_000_009, true);
    let mut player_sess = make_session(2, 11, 90_000_011, 1_000_009, false);
    let mut unauth_sess = SessionState::new(3);

    let fw_sys = 30_002_056u32; // Huola
    admin_sess.solar_system_id = Some(fw_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            fw_sys,
            FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: 500_003,
                occupier_faction_id: 500_003,
                victory_points: 100,
                victory_points_threshold: 3_000,
            },
        );
    });

    // 1. Unauthenticated & non-admin error cases
    let mut inc_req = Vec::new();
    encode_u64_id(1, u64::from(fw_sys), &mut inc_req);
    encode_u64_id(2, 500_003, &mut inc_req);
    encode_varint_field(3, 250, &mut inc_req);

    let r_unauth = quasar_rpc(&ctx, &mut unauth_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone()).await;
    assert_eq!(r_unauth.status_code, STATUS_UNAUTHORIZED);

    let r_forbid = quasar_rpc(&ctx, &mut player_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone()).await;
    assert_eq!(r_forbid.status_code, STATUS_FORBIDDEN);

    // 2. Unknown / non-warzone system -> 400 or 404
    let mut bad_sys = Vec::new();
    encode_u64_id(1, 99_999_999, &mut bad_sys);
    let r_not_found = quasar_rpc(&ctx, &mut admin_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", bad_sys).await;
    assert!(matches!(r_not_found.status_code, STATUS_BAD_REQUEST | STATUS_NOT_FOUND));

    // 3. Valid increment mutates victory points & emits notices
    let mut rx = subscribe_notices(&ctx, &mut admin_sess).await;

    let r_inc = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        inc_req,
    )
    .await;
    assert_eq!(r_inc.status_code, STATUS_OK);
    let vp_after = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.get(&fw_sys).unwrap().victory_points);
    assert_eq!(vp_after, 350);

    let notices = drain_notice_names(&mut rx);
    assert!(notices.contains(&"eve_public.faction.activity.ScoreContributionNotice".to_string()));
    assert!(notices.contains(&"eve_public.faction.activity.SolarSystemScoresNotice".to_string()));

    // 4. SetContributionScoreRequest overrides victory points
    let mut set_req = Vec::new();
    encode_u64_id(1, u64::from(fw_sys), &mut set_req);
    encode_u64_id(2, 500_002, &mut set_req);
    encode_varint_field(3, 1_500, &mut set_req);
    let r_set = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
        set_req,
    )
    .await;
    assert_eq!(r_set.status_code, STATUS_OK);
    let (vp_set, occ_fac) = ctx.db.with_snapshot(|s| {
        let o = s.fw_system_occupations.get(&fw_sys).unwrap();
        (o.victory_points, o.occupier_faction_id)
    });
    assert_eq!(vp_set, 1_500);
    assert_eq!(occ_fac, 500_002);

    // 5. Payment schedule GetRequest & GetActiveByTypeRequest
    let sched = create_user_payment_schedule(&ctx.db, 10, 1, "omega-30d", 30, ctx.now_unix());
    let mut get_sched = Vec::new();
    encode_u64_id(1, sched.id, &mut get_sched);
    let r_sched = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.payment.schedule.api.GetRequest",
        get_sched,
    )
    .await;
    assert_eq!(r_sched.status_code, STATUS_OK);
    let dyn_sched = decode_dyn(&ctx, &r_sched);
    match dyn_sched.get(1) {
        Some(DynamicValue::Message(attr)) => {
            assert_eq!(attr.get(4), Some(&DynamicValue::Uint32(30)));
        }
        other => panic!("Expected attributes message, got {other:?}"),
    }

    let mut by_type = Vec::new();
    encode_varint_field(1, 1, &mut by_type);
    let r_type = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        by_type,
    )
    .await;
    assert_eq!(r_type.status_code, STATUS_OK);
    let dyn_type = decode_dyn(&ctx, &r_type);
    match dyn_type.get(1) {
        Some(DynamicValue::Message(ident)) => {
            assert_eq!(ident.get(1), Some(&DynamicValue::Uint64(sched.id)));
        }
        other => panic!("Expected schedule identifier, got {other:?}"),
    }
}

#[tokio::test]
async fn test_b33_achievement_and_goal_admin_closure() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj
    let ctx = ServerContext::new();
    let cid = 90_000_020i64;
    let mut admin_sess = make_session(4, 20, cid, 1_000_009, true);
    let mut player_sess = make_session(5, 21, 90_000_021, 1_000_009, false);

    // 1. Achievement admin: validation + AddProgressRequest + CompleteRequest + ResetRequest
    let aid = ctx
        .quasar_gateway
        .progression
        .achievements
        .definitions
        .first()
        .expect("at least one achievement definition")
        .id;

    let mut add_ach = Vec::new();
    encode_ach_uuid_ident(1, aid, &mut add_ach);
    encode_varint_field(2, 5, &mut add_ach);

    let r_forbid = quasar_rpc(
        &ctx,
        &mut player_sess,
        "eve_public.achievement.api.admin.AddProgressRequest",
        add_ach.clone(),
    )
    .await;
    assert_eq!(r_forbid.status_code, STATUS_FORBIDDEN);

    let mut bad_ach = Vec::new();
    encode_ach_uuid_ident(1, 0, &mut bad_ach);
    let r_bad = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.achievement.api.admin.AddProgressRequest",
        bad_ach,
    )
    .await;
    assert_eq!(r_bad.status_code, STATUS_BAD_REQUEST);

    let r_ok = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.AddProgressRequest", add_ach).await;
    assert_eq!(r_ok.status_code, STATUS_OK);
    let ach_prog = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(cid as u32)
        .into_iter().find(|a| a.def.id == aid).unwrap().progress;
    assert_eq!(ach_prog, 5);

    let mut comp_ach = Vec::new();
    encode_ach_uuid_ident(1, aid, &mut comp_ach);
    let r_comp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.achievement.api.admin.CompleteRequest",
        comp_ach.clone(),
    )
    .await;
    assert_eq!(r_comp.status_code, STATUS_OK);

    let r_reset = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.achievement.api.admin.ResetRequest",
        comp_ach,
    )
    .await;
    assert_eq!(r_reset.status_code, STATUS_OK);

    let r_reset_all = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.achievement.api.admin.ResetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r_reset_all.status_code, STATUS_OK);

    // 2. Career goal admin: ProgressRequest + CompleteRequest
    let career_goals = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(cid as u32);
    let cg_uuid = career_goals.first().expect("career goal").uuid;

    let mut prog_cg = Vec::new();
    encode_uuid_id(1, &cg_uuid, &mut prog_cg);
    encode_varint_field(2, 10, &mut prog_cg);
    let r_cg = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.career.goal.api.admin.ProgressRequest",
        prog_cg,
    )
    .await;
    assert_eq!(r_cg.status_code, STATUS_OK);

    let mut comp_cg = Vec::new();
    encode_uuid_id(1, &cg_uuid, &mut comp_cg);
    let r_cg_comp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.career.goal.api.admin.CompleteRequest",
        comp_cg,
    )
    .await;
    assert_eq!(r_cg_comp.status_code, STATUS_OK);

    // 3. Daily goal admin: ProgressRequest
    let daily_goals = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals_at(cid as u32, ctx.now_unix());
    let dg_uuid = daily_goals.first().expect("daily goal").uuid;

    let mut prog_dg = Vec::new();
    encode_uuid_id(1, &dg_uuid, &mut prog_dg);
    encode_u64_id(2, cid as u64, &mut prog_dg);
    encode_varint_field(3, 25, &mut prog_dg);
    let r_dg = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        prog_dg,
    )
    .await;
    assert_eq!(r_dg.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_b33_sovereignty_hub_resources_and_fuel_closure() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    let cid = 90_000_030i64;
    let corp_id = 1_000_009i64;
    let hub_id = 1_050_000_001i64;
    let null_sys = 30_004_759u32; // 1DQ1-A
    let mut sess = make_session(6, 30, cid, corp_id, false);
    sess.solar_system_id = Some(null_sys);

    let fuel_item_id = 9_900_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        seed_char(snap, cid, 30, corp_id, null_sys);
        seed_structure(snap, hub_id, corp_id, null_sys, TYPE_SOVEREIGNTY_HUB);
        snap.sov_runtime
            .sov_hubs
            .insert(hub_id, PersistedSovHubData::default());
        snap.items.insert(
            fuel_item_id,
            eve_db::InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: TYPE_MAGMATIC_GAS,
                owner_id: corp_id,
                location_id: hub_id,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 1. GetResourcesRequest
    let mut res_req = Vec::new();
    encode_u64_id(1, hub_id as u64, &mut res_req);
    let r_res = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.hub.api.GetResourcesRequest",
        res_req.clone(),
    )
    .await;
    assert_eq!(r_res.status_code, STATUS_OK);
    let dyn_res = decode_dyn(&ctx, &r_res);
    assert!(dyn_res.get(1).is_some());
    assert!(dyn_res.get(2).is_some());

    // 2. Fuel AddRequest & GetRequest
    let mut add_fuel = Vec::new();
    encode_u64_id(1, fuel_item_id as u64, &mut add_fuel);
    encode_varint_field(2, 500, &mut add_fuel);
    encode_u64_id(3, hub_id as u64, &mut add_fuel);
    let r_add = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.hub.fuel.api.AddRequest",
        add_fuel,
    )
    .await;
    assert_eq!(r_add.status_code, STATUS_OK);

    let r_fuel = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.hub.fuel.api.GetRequest",
        res_req,
    )
    .await;
    assert_eq!(r_fuel.status_code, STATUS_OK);
    let fuel_qty = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .sov_hubs
            .get(&hub_id)
            .and_then(|h| h.fuel.get(&TYPE_MAGMATIC_GAS).copied())
            .unwrap_or(0)
    });
    assert_eq!(fuel_qty, 500);
}

#[tokio::test]
async fn test_b33_mercenary_den_activities_and_notices_closure() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
    let ctx = ServerContext::new();
    let cid = 90_000_040i64;
    let corp_id = 1_000_009i64;
    let den_id = 1_060_000_001i64;
    let null_sys = 30_004_759u32;
    let mut sess = make_session(7, 40, cid, corp_id, false);
    sess.solar_system_id = Some(null_sys);

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        seed_char(snap, cid, 40, corp_id, null_sys);
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                type_id: 85_230,
                owner_char_id: cid,
                skyhook_id: 1_070_000_099,
                solar_system_id: null_sys,
                planet_id: 40_301_001,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 10,
                anarchy_level: 0,
                infomorphs: 50,
                simulated_at_unix: now,
                next_activity_generation_unix: 0,
            },
        );
    });

    // 1. GetCapacityRequest
    let r_cap = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r_cap.status_code, STATUS_OK);
    let dyn_cap = decode_dyn(&ctx, &r_cap);
    assert!(matches!(dyn_cap.get(1), Some(DynamicValue::Uint32(c)) if *c > 0));

    // 2. GetForMercenaryDenRequest generates activities & emits AddedNotice
    let mut rx = subscribe_notices(&ctx, &mut sess).await;

    let mut get_den = Vec::new();
    encode_u64_id(1, den_id as u64, &mut get_den);
    let r_acts = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
        get_den,
    )
    .await;
    assert_eq!(r_acts.status_code, STATUS_OK);

    let act_uuid = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .mercenary_activities
            .values()
            .find(|a| a.den_id == den_id)
            .expect("generated activity")
            .uuid
            .clone()
    });
    let notices = drain_notice_names(&mut rx);
    assert!(notices.contains(
        &"eve_public.sovereignty.mercenaryden.activity.api.AddedNotice".to_string()
    ));

    // 3. GetAllRequest
    let r_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r_all.status_code, STATUS_OK);

    // 4. StartRequest mutates activity state & emits StartedNotice + ExpiryChangedNotice
    let mut start_req = Vec::new();
    encode_uuid_id(1, &act_uuid, &mut start_req);
    let r_start = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
        start_req,
    )
    .await;
    assert_eq!(r_start.status_code, STATUS_OK);
    let started = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .mercenary_activities
            .values()
            .any(|a| a.den_id == den_id && a.started)
    });
    assert!(started);
}

#[tokio::test]
async fn test_b33_skyhook_admin_closure() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
    let ctx = ServerContext::new();
    let (cid, corp_id, skyhook_id, sys_id) = (90_000_050i64, 1_000_009i64, 1_070_000_001i64, 30_000_142u32);
    let mut player_sess = make_session(9, 50, cid, corp_id, false);
    let mut admin_sess = make_session(8, 50, cid, corp_id, true);

    ctx.db.with_mut_snapshot(|snap| {
        seed_char(snap, cid, 50, corp_id, sys_id);
        seed_structure(snap, skyhook_id, corp_id, sys_id, TYPE_ORBITAL_SKYHOOK);
    });

    // Non-admin player Director must be rejected with STATUS_FORBIDDEN on both admin endpoints
    let mut mod_req = Vec::new();
    encode_u64_id(1, skyhook_id as u64, &mut mod_req);
    let r_mod_forbidden = quasar_rpc(&ctx, &mut player_sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await;
    assert_eq!(r_mod_forbidden.status_code, STATUS_FORBIDDEN);

    let now = ctx.now_unix();
    let mut ts_start = Vec::new();
    encode_varint_field(1, now + 60, &mut ts_start);
    let mut ts_end_ok = Vec::new();
    encode_varint_field(1, now + 3_660, &mut ts_end_ok);
    let mut good_win = Vec::new();
    encode_u64_id(1, skyhook_id as u64, &mut good_win);
    encode_bytes_field(2, &ts_start, &mut good_win);
    encode_bytes_field(3, &ts_end_ok, &mut good_win);

    let r_win_forbidden = quasar_rpc(&ctx, &mut player_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", good_win.clone()).await;
    assert_eq!(r_win_forbidden.status_code, STATUS_FORBIDDEN);

    // Admin caller succeeds on ModifyReagentsRequest & ScheduleTheftVulnerabilityWindowRequest
    let r_mod_ok = quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req).await;
    assert_eq!(r_mod_ok.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagents.get(&skyhook_id).copied()), Some((TYPE_MAGMATIC_GAS, 1200)));

    let mut bad_win = Vec::new();
    encode_u64_id(1, skyhook_id as u64, &mut bad_win);
    encode_bytes_field(2, &ts_start, &mut bad_win);
    let mut ts_end_bad = Vec::new();
    encode_varint_field(1, now + 200_000, &mut ts_end_bad);
    encode_bytes_field(3, &ts_end_bad, &mut bad_win);
    assert_eq!(quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", bad_win).await.status_code, STATUS_BAD_REQUEST);

    let r_good_win = quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", good_win).await;
    assert_eq!(r_good_win.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied()), Some((now + 60, now + 3_660)));
}
