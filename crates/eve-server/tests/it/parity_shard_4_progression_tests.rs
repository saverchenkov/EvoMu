//! Integration tests for AIR achievements, AIR career program, campaign SDE
//! definitions, daily goals, and Aura AI assistant chat.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const PARAGON_CORP: i64 = 1_000_419;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

fn sess(sid: u64, uid: u64, cid: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(JITA_SYS);
    s.station_id = Some(JITA_STATION);
    s.role = role;
    s
}

fn seed_pilot(db: &WorldRepository, cid: i64, uid: u64, name: &str, isk_cents: i64, evermarks: i64) {
    db.with_mut_snapshot(|snap| {
        let uname = format!("user_{uid}");
        snap.accounts.entry(uname.clone()).or_insert_with(|| AccountRecord {
            user_id: uid,
            username: uname,
            password_token: "hash".into(),
            role: service_roles::ROLE_PLAYER_DEFAULT,
            banned_until_unix: None,
            ban_reason: None,
        });
        let mut lp = HashMap::new();
        lp.insert(PARAGON_CORP, evermarks);
        snap.characters.insert(cid, CharacterRecord {
            character_id: cid,
            user_id: uid,
            name: name.into(),
            corporation_id: 98_000_001,
            alliance_id: None,
            war_faction_id: None,
            station_id: Some(JITA_STATION),
            structure_id: None,
            solar_system_id: JITA_SYS,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: cid + 50_000,
            balance: IskCents(isk_cents),
            plex_balance: 50,
            loyalty_points: lp,
            standings: HashMap::new(),
            security_status: 5.0,
            clone_grade: 2,
            home_station_id: JITA_STATION,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 11,
            school_id: 17,
            neural: NeuralAttributes::default(),
            skill_points: 5_000_000.0,
            activity_status: None,
            free_skill_points: 10_000.0,
            last_clone_jump_unix: 0,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: String::new(),
            title: String::new(),
            gender: 0,
            create_date_unix: 1_704_067_200,
            fleet_id: None,
            autopilot_waypoints: Vec::new(),
            version: 1,
            system_epoch: 1,
        });
        snap.items.insert(cid + 50_000, InventoryItemRecord {
            item_id: cid + 50_000,
            type_id: 587,
            owner_id: cid,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
    });
}

async fn qrpc(ctx: &ServerContext, s: &mut SessionState, ty: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: vec![3; 16],
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: vec![4; 16],
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(kv) = &rep else { panic!("expected KeyVal, got {rep:?}"); };
    let raw = kv.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
        _ => None,
    }).expect("response_proto");
    EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse")
}

async fn subscribe(ctx: &ServerContext, s: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(s.session_id).expect("stream rx")
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p.normalized_type_name().to_string());
        }
    }
    out
}

fn id_msg(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

fn uuid_msg(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

// parity: feat:quasar.achievement
// parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn achievements_sde_rewards_showcase_and_admin_progression() {
    let db_path = std::env::temp_dir().join(format!("evomu_shard4_ach_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite open"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    common::pin_clock(&ctx);

    let cid = 2_115_200_001i64;
    let admin_id = 2_115_200_099i64;
    seed_pilot(&db, cid, 5201, "AchieverPilot", 10_000_000_00, 15_000);
    seed_pilot(&db, admin_id, 5299, "AchieverAdmin", 10_000_000_00, 0);

    let mut player = sess(21, 5201, cid, service_roles::ROLE_PLAYER_DEFAULT);
    let _admin = sess(29, 5299, admin_id, service_roles::ROLE_GM_DEFAULT);
    let mut rx = subscribe(&ctx, &mut player).await;

    let achs = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(cid as u32);
    assert!(!achs.is_empty(), "achievement manager must populate SDE definitions");
    let rewarded = achs.iter().find(|a| a.def.reward_isk > 0 || a.def.reward_sp > 0).expect("SDE achievement with positive reward");
    let aid = rewarded.def.id;
    let cat_id = rewarded.def.category_id.max(1);

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new()).await.status_code, STATUS_OK);
    let mut get_ach = Vec::new();
    id_msg(1, u64::from(aid), &mut get_ach);
    let ach_rsp = qrpc(&ctx, &mut player, "eve_public.achievement.api.GetRequest", get_ach.clone()).await;
    assert_eq!(ach_rsp.status_code, STATUS_OK);
    let ach_wm = WireMessage::decode(&ach_rsp.payload.unwrap().value).unwrap();
    assert_ne!(ach_wm.fields.len(), 0);

    let mut get_cat = Vec::new();
    id_msg(1, u64::from(cat_id), &mut get_cat);
    let cat_rsp = qrpc(&ctx, &mut player, "eve_public.achievement.category.api.GetRequest", get_cat).await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    let cat_wm = WireMessage::decode(&cat_rsp.payload.unwrap().value).unwrap();
    assert_ne!(cat_wm.fields.len(), 0);

    let score_before_rsp = qrpc(&ctx, &mut player, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new()).await;
    assert_eq!(score_before_rsp.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new()).await.status_code, STATUS_OK);

    assert_eq!(
        qrpc(&ctx, &mut player, "eve_public.achievement.api.admin.CompleteRequest", get_ach).await.status_code,
        STATUS_FORBIDDEN
    );
    let comp = ctx.quasar_gateway.progression.achievements.mark_completed(cid as u32, aid, ctx.now_unix()).expect("complete achievement");
    assert_eq!(comp.completed, true);
    assert_eq!(comp.def.id, aid);
    ctx.quasar_gateway.progression.achievements.save_to_db();

    let score_after_rsp = qrpc(&ctx, &mut player, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new()).await;
    let score_after_wm = WireMessage::decode(&score_after_rsp.payload.unwrap().value).unwrap();
    let total_score = score_after_wm.fields.iter().find_map(|f| f.value.as_varint()).unwrap_or(0);
    assert!(total_score >= 1, "completed achievement must contribute positive showcase score, got {total_score}");

    assert_eq!(
        qrpc(&ctx, &mut player, "eve_public.achievement.reward.api.ClaimRequest", Vec::new()).await.status_code,
        STATUS_PRECONDITION_FAILED
    );

    let isk_before = db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_before = db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    let claimed = ctx.quasar_gateway.progression.achievements.claim_rewards(cid as u32, &db);
    assert!(claimed >= 1);
    let isk_after = db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_after = db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    assert!(isk_after > isk_before || sp_after > sp_before, "reward claim must credit ISK or SP");

    let _ = drain_notices(&mut rx);

    drop(ctx);
    drop(db);
    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen sqlite");
    reopened.with_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].balance.0, isk_after);
        assert_eq!(snap.characters[&cid].free_skill_points, sp_after);
    });
    let _ = std::fs::remove_file(&db_path);
}

// parity: feat:pve.air-career-program
// parity: feat:quasar.career
// parity: quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn air_career_program_and_campaign_definitions_sde() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 2_115_200_011i64;
    seed_pilot(&ctx.db, cid, 5211, "CareerPilot", 5_000_000_00, 10_000);
    let mut player = sess(22, 5211, cid, service_roles::ROLE_PLAYER_DEFAULT);
    let mut rx = subscribe(&ctx, &mut player).await;

    let defs_rsp = qrpc(&ctx, &mut player, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new()).await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    let defs_wm = WireMessage::decode(&defs_rsp.payload.unwrap().value).unwrap();
    assert_ne!(defs_wm.fields.len(), 0);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.career.goal.api.admin.ProgressRequest", Vec::new()).await.status_code, STATUS_FORBIDDEN);

    let goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(cid as u32);
    assert!(goals.len() >= 1);
    let first_goal = &goals[0];
    let goal_uuid = first_goal.uuid;
    let activity_kind = first_goal.def.activity_kind.clone();
    let target = first_goal.def.target.max(1);
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_gameplay_event(cid as u32, &activity_kind, target, &ctx.quasar_gateway);

    let isk_before = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_before = ctx.db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    let mut claim_req = Vec::new();
    uuid_msg(1, &goal_uuid, &mut claim_req);
    let claim_rsp = qrpc(&ctx, &mut player, "eve_public.career.goal.api.ClaimRewardRequest", claim_req.clone()).await;
    assert_eq!(claim_rsp.status_code, STATUS_OK, "career ClaimRewardRequest failed: {}", claim_rsp.status_message);

    let updated_goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(cid as u32);
    let updated_goal = updated_goals.iter().find(|g| g.uuid == goal_uuid).expect("updated career goal");
    assert_eq!(updated_goal.claimed, true);
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_after = ctx.db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    assert!(isk_after >= isk_before && sp_after >= sp_before, "claiming career goal must preserve or credit balances");

    let dup_claim = qrpc(&ctx, &mut player, "eve_public.career.goal.api.ClaimRewardRequest", claim_req).await;
    let dup_code = dup_claim.status_code;
    assert_ne!(dup_code, 200);

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n.contains("career.goal.api.")), "expected career goal notice, got {notices:?}");

    let cm_rsp = qrpc(&ctx, &mut player, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new()).await;
    assert_eq!(cm_rsp.status_code, STATUS_OK);
    let cm_wm = WireMessage::decode(&cm_rsp.payload.unwrap().value).unwrap();
    assert_ne!(cm_wm.fields.len(), 0);
    let em_rsp = qrpc(&ctx, &mut player, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new()).await;
    assert_eq!(em_rsp.status_code, STATUS_OK);
    let em_wm = WireMessage::decode(&em_rsp.payload.unwrap().value).unwrap();
    assert_ne!(em_wm.fields.len(), 0);
}

// parity: feat:pve.daily-goals
// parity: feat:quasar.dailygoal
// parity: quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daily_goals_evermark_redemption_and_ai_assistant() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 2_115_200_021i64;
    let admin_id = 2_115_200_029i64;
    seed_pilot(&ctx.db, cid, 5221, "DailyPilot", 5_000_000_00, 30_000);
    seed_pilot(&ctx.db, admin_id, 5229, "DailyAdmin", 5_000_000_00, 0);

    let mut player = sess(23, 5221, cid, service_roles::ROLE_PLAYER_DEFAULT);
    let mut admin = sess(24, 5229, admin_id, service_roles::ROLE_GM_DEFAULT);
    let mut rx = subscribe(&ctx, &mut player).await;

    let cur_rsp = qrpc(&ctx, &mut player, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(cur_rsp.status_code, STATUS_OK);
    let goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(cid as u32);
    assert!(!goals.is_empty());
    let g_uuid = goals[0].uuid;

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.dailygoal.api.admin.ProgressRequest", Vec::new()).await.status_code, STATUS_FORBIDDEN);
    let mut prog_req = Vec::new();
    uuid_msg(1, &g_uuid, &mut prog_req);
    encode_varint_field(2, cid as u64, &mut prog_req);
    encode_varint_field(3, goals[0].template.target.max(1) as u64, &mut prog_req);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.dailygoal.api.admin.ProgressRequest", prog_req).await.status_code, STATUS_OK);

    let isk_before = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    let mut redeem_req = Vec::new();
    uuid_msg(1, &g_uuid, &mut redeem_req);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.dailygoal.api.RedeemRequest", redeem_req).await.status_code, STATUS_OK);
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    assert!(isk_after >= isk_before);

    if let Some(second) = goals.get(1) {
        let mut pay_req = Vec::new();
        uuid_msg(1, &second.uuid, &mut pay_req);
        assert_eq!(qrpc(&ctx, &mut player, "eve_public.dailygoal.api.PayForCompletionRequest", pay_req).await.status_code, STATUS_OK);
    }

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n.contains("dailygoal.api.")), "expected daily goal notice, got {notices:?}");

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.ai.assistant.chat.api.QueryRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    let mut ai_req = Vec::new();
    encode_string_field(1, "Where is Jita located?", &mut ai_req);
    let ai_rsp = qrpc(&ctx, &mut player, "eve_public.ai.assistant.chat.api.QueryRequest", ai_req).await;
    assert_eq!(ai_rsp.status_code, STATUS_OK);
    assert!(!ai_rsp.payload.unwrap().value.is_empty());
}
