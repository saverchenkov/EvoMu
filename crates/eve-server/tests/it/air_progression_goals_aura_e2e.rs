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

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push((p.normalized_type_name().to_string(), p.value));
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
    assert!(
        achs.iter().all(|a| a.def.reward_isk == 0 && a.def.reward_sp == 0 && a.def.score == 0 && (1..=5).contains(&a.def.category_id)),
        "default client achievements.static definitions assign DERIVED categories 1..=5 while omitting scores and rewards"
    );
    let sample = &achs[0];
    let aid = sample.def.id;
    let cat_id = sample.def.category_id;

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
    assert_eq!(total_score, 0, "default unconfigured achievement must contribute 0 showcase score");

    assert_eq!(
        qrpc(&ctx, &mut player, "eve_public.achievement.reward.api.ClaimRequest", Vec::new()).await.status_code,
        STATUS_PRECONDITION_FAILED
    );

    let isk_before = db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_before = db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    let claimed = ctx.quasar_gateway.progression.achievements.claim_rewards(cid as u32, &db);
    assert_eq!(claimed, 0, "unrewarded client achievement must fail closed without phantom payout");
    let isk_after = db.with_snapshot(|s| s.characters[&cid].balance.0);
    let sp_after = db.with_snapshot(|s| s.characters[&cid].free_skill_points);
    assert_eq!(isk_after, isk_before, "unrewarded achievement must not alter ISK");
    assert_eq!(sp_after, sp_before, "unrewarded achievement must not alter SP");

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
    let cg_prog_bytes = notices
        .iter()
        .find(|(t, _)| t == "eve_public.career.goal.api.ProgressedNotice")
        .map(|(_, v)| v.as_slice())
        .expect("expected career.goal.api.ProgressedNotice");
    let cg_prog_wm = WireMessage::decode(cg_prog_bytes).expect("decode career ProgressedNotice");
    let cg_prog_id = WireMessage::decode(cg_prog_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(cg_prog_id.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()), Some(goal_uuid.as_slice()));
    assert_eq!(cg_prog_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(target));
    let cg_comp_bytes = notices
        .iter()
        .find(|(t, _)| t == "eve_public.career.goal.api.CompletedNotice")
        .map(|(_, v)| v.as_slice())
        .expect("expected career.goal.api.CompletedNotice");
    let cg_comp_wm = WireMessage::decode(cg_comp_bytes).expect("decode career CompletedNotice");
    let cg_comp_id = WireMessage::decode(cg_comp_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(cg_comp_id.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()), Some(goal_uuid.as_slice()));

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
    let dg_comp_bytes = notices
        .iter()
        .find(|(t, _)| t == "eve_public.dailygoal.api.CompletedNotice")
        .map(|(_, v)| v.as_slice())
        .expect("expected dailygoal.api.CompletedNotice");
    let dg_comp_wm = WireMessage::decode(dg_comp_bytes).expect("decode dailygoal CompletedNotice");
    let dg_comp_id = WireMessage::decode(dg_comp_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(dg_comp_id.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()), Some(g_uuid.as_slice()));
    let dg_red_bytes = notices
        .iter()
        .find(|(t, _)| t == "eve_public.dailygoal.api.RedeemedNotice")
        .map(|(_, v)| v.as_slice())
        .expect("expected dailygoal.api.RedeemedNotice");
    let dg_red_wm = WireMessage::decode(dg_red_bytes).expect("decode dailygoal RedeemedNotice");
    let dg_red_id = WireMessage::decode(dg_red_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(dg_red_id.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()), Some(g_uuid.as_slice()));

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.ai.assistant.chat.api.QueryRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    let mut ai_req = Vec::new();
    encode_string_field(1, "Where is Jita located?", &mut ai_req);
    let ai_rsp = qrpc(&ctx, &mut player, "eve_public.ai.assistant.chat.api.QueryRequest", ai_req).await;
    assert_eq!(ai_rsp.status_code, STATUS_OK);
    assert!(!ai_rsp.payload.unwrap().value.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn achievements_cat5_and_live_daily_damage_ships_capsuleer_vs_npc() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let atk_cid = 2_115_200_101i64;
    let def_cid = 2_115_200_102i64;
    let atk_ship_id = atk_cid + 50_000;
    let def_ship_id = def_cid + 50_000;
    let npc_ship_id = 2_115_900_999i64;
    let gun_id = 2_115_900_101i64;
    let ammo_id = 2_115_900_102i64;

    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.05 && s.security_status < 0.40)
        .expect("SDE lowsec system")
        .solar_system_id;

    seed_pilot(&ctx.db, atk_cid, 5301, "DamageAttacker", 10_000_000_00, 1_000);
    seed_pilot(&ctx.db, def_cid, 5302, "DamageDefender", 10_000_000_00, 1_000);

    let mut atk_sess = sess(31, 5301, atk_cid, service_roles::ROLE_PLAYER_DEFAULT);
    atk_sess.station_id = None;
    atk_sess.solar_system_id = Some(low_sys);
    atk_sess.ship_id = Some(atk_ship_id);

    // 1. Achievement Category 5 (Social) query succeeds and CDN definitions build cleanly.
    let mut cat5_req = Vec::new();
    id_msg(1, 5, &mut cat5_req);
    let cat5_rsp = qrpc(&ctx, &mut atk_sess, "eve_public.achievement.category.api.GetRequest", cat5_req).await;
    assert_eq!(cat5_rsp.status_code, STATUS_OK);
    assert_eq!(
        qrpc(&ctx, &mut atk_sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new())
            .await
            .status_code,
        STATUS_OK
    );

    // 2. Spawn attacker, defender capsuleer ship, and an NPC ship in lowsec.
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(atk_cid, atk_ship_id), (def_cid, def_ship_id)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.solar_system_id = low_sys;
            }
            if let Some(ship) = snap.items.get_mut(&sid) {
                ship.location_id = i64::from(low_sys);
                ship.flag = InventoryFlag::None;
            }
        }
        snap.items.insert(
            npc_ship_id,
            InventoryItemRecord {
                item_id: npc_ship_id,
                type_id: 587,
                owner_id: 1_000_001,
                location_id: i64::from(low_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 2881,
                owner_id: atk_cid,
                location_id: atk_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 21898,
                owner_id: atk_cid,
                location_id: atk_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(gun_id, true);
        let sk = snap.skills.entry(atk_cid).or_default();
        for sid in [3300_u32, 3302, 3311, 3413, 3426] {
            sk.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(atk_ship_id);

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None)
        .await
        .expect("SetSafetyLevel Red");

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    for (bid, pos_x, cs, meta) in [
        (
            atk_ship_id,
            0.0,
            ctx.build_ship_combat_state(atk_cid, atk_ship_id, 587),
            ctx.build_ship_slim_meta(atk_cid, 587),
        ),
        (
            npc_ship_id,
            1_000.0,
            eve_destiny::CombatState::new(),
            eve_destiny::SlimItemMeta::default(),
        ),
        (
            def_ship_id,
            1_200.0,
            ctx.build_ship_combat_state(def_cid, def_ship_id, 587),
            ctx.build_ship_slim_meta(def_cid, 587),
        ),
    ] {
        let _ = sys_h
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: bid,
                    type_id: 587,
                    position: glam::DVec3::new(pos_x, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_067_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
                meta,
            )
            .await;
    }

    let damage_goal_prog = || {
        ctx.quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals(atk_cid as u32)
            .into_iter()
            .find(|g| g.template.category == 7)
            .expect("DamageShips daily goal")
            .current_progress
    };
    assert_eq!(damage_goal_prog(), 0);

    // 3. Fire turret at NPC ball -> must NOT increment DamageShips daily goal.
    ctx.dispatch_rpc(&mut atk_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_ship_id)], &kw, None)
        .await
        .expect("AddTarget NPC");
    for _ in 0..30 {
        if sys_h.get_target_locks(atk_ship_id).await.unwrap().iter().any(|l| l.target_id == npc_ship_id && l.locked) {
            break;
        }
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(
        &mut atk_sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::Integer(npc_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate turret on NPC");
    let _ = ctx
        .dispatch_rpc(&mut atk_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gun_id)], &kw, None)
        .await;
    assert_eq!(damage_goal_prog(), 0, "NPC ship damage must never credit DamageShips");

    // 4. Fire turret at capsuleer ship -> MUST increment DamageShips daily goal > 0.
    ctx.dispatch_rpc(&mut atk_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(def_ship_id)], &kw, None)
        .await
        .expect("AddTarget capsuleer");
    for _ in 0..30 {
        if sys_h.get_target_locks(atk_ship_id).await.unwrap().iter().any(|l| l.target_id == def_ship_id && l.locked) {
            break;
        }
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(
        &mut atk_sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::Integer(def_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate turret on capsuleer ship");
    let _ = ctx
        .dispatch_rpc(&mut atk_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gun_id)], &kw, None)
        .await;
    assert!(damage_goal_prog() > 0, "Capsuleer ship damage must credit DamageShips daily goal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn career_program_live_rpc_duels_social_fw_and_market() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let c1 = 2_115_200_201i64;
    let c2 = 2_115_200_202i64;
    let pyerite_item_id = 2_115_900_201i64;
    seed_pilot(&ctx.db, c1, 5401, "CareerTrader1", 50_000_000_00, 1_000);
    seed_pilot(&ctx.db, c2, 5402, "CareerTrader2", 50_000_000_00, 1_000);

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 35);
        snap.items.insert(
            pyerite_item_id,
            InventoryItemRecord {
                item_id: pyerite_item_id,
                type_id: 35,
                owner_id: c1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut s1 = sess(41, 5401, c1, service_roles::ROLE_PLAYER_DEFAULT);
    s1.region_id = Some(10_000_002);
    s1.constellation_id = Some(20_000_020);
    let mut s2 = sess(42, 5402, c2, service_roles::ROLE_PLAYER_DEFAULT);
    s2.region_id = Some(10_000_002);
    s2.constellation_id = Some(20_000_020);
    let _ = ctx
        .db
        .acquire_online_lock(5401, 41, Some(c1), false, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(5402, 42, Some(c2), false, ctx.now_unix());
    let kw = HashMap::new();

    // 1. Social RPCs: AddContact, SendMail, JoinChannel + SendMessage
    ctx.dispatch_rpc(
        &mut s1,
        "charMgr",
        None,
        "AddContact",
        &[PyRep::Integer(c2), PyRep::Real(5.0), PyRep::Bool(false), PyRep::Bool(false), PyRep::String("wing".into())],
        &kw,
        None,
    )
    .await
    .expect("AddContact");

    ctx.dispatch_rpc(
        &mut s1,
        "mailMgr",
        None,
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("Hello".into()), PyRep::String("Fly safe".into())],
        &kw,
        None,
    )
    .await
    .expect("SendMail");

    ctx.dispatch_rpc(
        &mut s1,
        "LSC",
        None,
        "JoinChannel",
        &[PyRep::String("player_career_it".into())],
        &kw,
        None,
    )
    .await
    .expect("JoinChannel");
    ctx.dispatch_rpc(
        &mut s1,
        "LSC",
        None,
        "SendMessage",
        &[PyRep::String("player_career_it".into()), PyRep::String("o7".into())],
        &kw,
        None,
    )
    .await
    .expect("SendMessage");

    // 2. FW Direct Enlistment
    ctx.dispatch_rpc(
        &mut s1,
        "fwCharacterEnlistmentMgr",
        None,
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
        &kw,
        None,
    )
    .await
    .expect("CreateMyDirectEnlistment");

    // 3. Market sell + instant buy -> seller earn_isk, buyer spend_isk (whole ISK = 50,000)
    let sell_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(pyerite_item_id)),
        ("typeID", PyRep::Integer(35)),
        ("price", PyRep::Real(100.0)),
        ("quantity", PyRep::Integer(1_000)),
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("duration", PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_arg, PyRep::Bool(false)],
        &kw,
        None,
    )
    .await
    .expect("PlaceMultiSellOrder");

    let buy_arg = PyRep::List(vec![PyRep::keyval([
        ("typeID", PyRep::Integer(35)),
        ("quantity", PyRep::Integer(500)),
        ("maxPrice", PyRep::Real(100.0)),
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("orderRange", PyRep::Integer(-1)),
    ])]);
    let buy_res = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[buy_arg, PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("BuyMultipleItems");
    assert_eq!(buy_res.dict_get("purchasedCount").and_then(PyRep::as_i64), Some(1));

    // 4. Undock both pilots into space in JITA_SYS for Duels
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [c1, c2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.solar_system_id = JITA_SYS;
            }
            let sid = cid + 50_000;
            if let Some(ship) = snap.items.get_mut(&sid) {
                ship.location_id = i64::from(JITA_SYS);
                ship.flag = InventoryFlag::None;
            }
        }
    });
    for (idx, cid) in [c1, c2].into_iter().enumerate() {
        let sid = cid + 50_000;
        let _ = sys_h
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: glam::DVec3::new((idx as f64) * 1_000.0, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_067_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, sid, 587),
                ctx.build_ship_slim_meta(cid, 587),
            )
            .await;
    }
    s1.station_id = None;
    s1.ship_id = Some(c1 + 50_000);
    s2.station_id = None;
    s2.ship_id = Some(c2 + 50_000);

    ctx.dispatch_rpc(&mut s1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)], &kw, None)
        .await
        .expect("StartDuelChallenge");
    ctx.dispatch_rpc(&mut s2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(c1), PyRep::Bool(true)], &kw, None)
        .await
        .expect("RespondToDuelChallenge");

    let prog_of = |cid: i64, lbl: &str| -> u64 {
        ctx.quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(cid as u32)
            .into_iter()
            .find(|g| g.def.goal_label == lbl)
            .unwrap_or_else(|| panic!("missing goal {lbl}"))
            .progress
    };
    assert_eq!(prog_of(c1, "SofAddContact"), 1);
    assert_eq!(prog_of(c1, "SofSendMail"), 1);
    assert_eq!(prog_of(c1, "SofSendChatMessage"), 1);
    assert_eq!(prog_of(c1, "SofJoinFW"), 1);
    assert_eq!(prog_of(c1, "IndustrialistEarnMarketIsk50k"), 50_000);
    assert_eq!(prog_of(c2, "EnforcerSpendMarketIsk50k"), 50_000);
    assert_eq!(prog_of(c1, "SofDuelChallenge"), 1);
    assert_eq!(prog_of(c1, "SofDuelWin1"), 1);
    assert_eq!(prog_of(c2, "SofDuelWin1"), 1);
}
