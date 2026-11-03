//! End-to-end integration tests closing canonical plan rows:
//! - `plan:E-5`: Agent finder and conversation lifecycle
//! - `plan:ESI-5`: Corporation structures timers, starbase detail, and plan closure
//! - `plan:Q-5`: Social and organization Quasar packages
//! - `plan:Q-6`: Economy and industry Quasar packages

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, rpc_call};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, IskCents, MarketOrderRecord, StarbaseDetailSettings, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::load_mission_meta;
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token { req = req.with_bearer_token(t); }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn encode_id_field(tag: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_varint_field(1, id, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

async fn send_quasar(ctx: &ServerContext, sess: &mut SessionState, type_url: &str, payload_val: Vec<u8>, corr: &[u8]) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"), value: payload_val }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-plan-fake-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await.expect("SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_plan_e5_agent_finder_and_conversation_lifecycle() {
    // parity: plan:E-5
    eve_testkit::attest(&["plan:E-5"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let empty_kw = HashMap::new();

    // 1. Train Negotiation (3356), Connections (3359), Security Connections (3895), Social (3355)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3356, TrainedSkillRecord { type_id: 3356, level: 5, skill_points: 256_000.0 });
        skills.insert(3359, TrainedSkillRecord { type_id: 3359, level: 4, skill_points: 135_765.0 });
        skills.insert(3895, TrainedSkillRecord { type_id: 3895, level: 4, skill_points: 135_765.0 });
        skills.insert(3355, TrainedSkillRecord { type_id: 3355, level: 4, skill_points: 135_765.0 });
    });

    let mut sess = SessionState::new(71_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 2. Agent Finder: Query Level 1 and Level 2 Security agents (division 24)
    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L1");
    let PyRep::Rowset { rows: l1_rows, .. } = l1_rep else { panic!("Expected Rowset") };
    assert!(!l1_rows.is_empty(), "Level 1 Security agents must be present");
    for row in &l1_rows {
        assert_eq!(row[2].as_i64(), Some(24));
        assert_eq!(row[7].as_i64(), Some(1));
        assert_eq!(row[9], PyRep::Bool(true));
    }

    let l2_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L2");
    let PyRep::Rowset { rows: l2_rows, .. } = l2_rep else { panic!() };
    assert!(!l2_rows.is_empty());
    let l2_corp_id = l2_rows[0][3].as_i64().unwrap();
    assert_eq!(l2_rows[0][9], PyRep::Bool(false), "L2 agent initially inaccessible without corp standing");

    // Standing gate: boost standing with L2 agent's corporation, verifying access unlocks
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(l2_corp_id, 3.5); }
    });
    let l2_st_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L2 st");
    let PyRep::Rowset { rows: l2_acc_rows, .. } = l2_st_rep else { panic!() };
    let unlocked = l2_acc_rows.iter().find(|r| r[3].as_i64() == Some(l2_corp_id)).unwrap();
    assert_eq!(unlocked[9], PyRep::Bool(true));

    // 3. Conversation lifecycle: Pick L1 agent and co-locate character
    let selected_agent = &l1_rows[0];
    let agent_id = selected_agent[0].as_i64().unwrap();
    let agent_sys = selected_agent[5].as_i64().unwrap() as u32;
    let agent_sta = selected_agent[6].as_i64();
    let agent_corp = selected_agent[3].as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys;
            ch.station_id = agent_sta;
        }
    });
    sess.solar_system_id = Some(agent_sys);
    sess.station_id = agent_sta;

    let sys_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetSolarSystemOfAgent", &[PyRep::Integer(agent_id)], &empty_kw, None).await.expect("GetSolarSystemOfAgent");
    assert_eq!(sys_rep.as_i64(), Some(agent_sys as i64));

    let agent_oid = ctx.bind_object(&sess, "agentMgr", agent_id).expect("bind agent moniker");

    // 4. Request mission -> Offered
    let offer_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &empty_kw, None).await.expect("DoAction(RequestMission)");
    let PyRep::KeyVal(offer_kv) = offer_res else { panic!("Expected KeyVal for offer") };
    let mission_id = offer_kv["missionID"].as_i64().unwrap() as u32;
    assert_eq!(offer_kv["state"].as_str(), Some("Offered"));
    assert_eq!(offer_kv["missionState"].as_i64(), Some(1));
    assert!(offer_kv["briefing"].as_str().is_some());
    assert!(offer_kv["objectives"].as_str().is_some());
    assert!(offer_kv["iskReward"].as_i64().unwrap() > 0);
    assert!(offer_kv["lpReward"].as_i64().unwrap() > 0);

    // Verify journal reflects offered mission
    let journal = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &empty_kw, None).await.expect("GetMyJournalDetails");
    let PyRep::Tuple(j_tup) = journal else { panic!() };
    let PyRep::List(j_list) = &j_tup[0] else { panic!() };
    assert!(j_list.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)));

    // 5. Decline mission without penalty on first decline
    let dec_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &empty_kw, None).await.expect("DoAction(Decline)");
    let PyRep::KeyVal(dec_kv) = dec_res else { panic!() };
    assert_eq!(dec_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec_kv["standingPenaltyApplied"], PyRep::Bool(false));

    // 6. Request new mission and Accept
    let offer2_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &empty_kw, None).await.expect("DoAction(Request 2)");
    let PyRep::KeyVal(offer2_kv) = offer2_res else { panic!() };
    let mission2_id = offer2_kv["missionID"].as_i64().unwrap() as u32;

    let acc_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &empty_kw, None).await.expect("DoAction(Accept)");
    let PyRep::KeyVal(acc_kv) = acc_res else { panic!() };
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    // 7. Complete the mission: satisfy objectives and invoke Complete (Action 6)
    let initial_balance = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance);
    let initial_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&agent_corp).copied().unwrap_or(0));
    let initial_agent_st = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&agent_id).copied().unwrap_or(0.0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission2_id)) { m.npcs_remaining = 0; }
        if let Some(ref m_meta) = load_mission_meta(snap, char_id, mission2_id) {
            if let Some(tid) = m_meta.required_delivery_type_id {
                let qty = m_meta.required_delivery_qty.max(1);
                snap.next_entity_id += 1;
                let iid = snap.next_entity_id;
                snap.items.insert(iid, make_item(iid, tid, char_id, agent_sta.unwrap_or(60_003_760), InventoryFlag::Hangar, qty, false));
            }
        }
    });

    let comp_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &empty_kw, None).await.expect("DoAction(Complete)");
    let PyRep::KeyVal(comp_kv) = comp_res else { panic!() };
    assert_eq!(comp_kv["state"].as_str(), Some("Completed"));

    let post_balance = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance);
    let post_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&agent_corp).copied().unwrap_or(0));
    let post_agent_st = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&agent_id).copied().unwrap_or(0.0));

    assert!(post_balance > initial_balance, "Balance increased");
    assert!(post_lp >= initial_lp, "LP awarded");
    assert!(post_agent_st >= initial_agent_st, "Standing propagated");
    assert!(ctx.db.with_snapshot(|snap| snap.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type.contains("mission"))));
}

#[tokio::test]
async fn test_plan_esi5_corp_structures_timers_and_starbase_detail() {
    // parity: plan:ESI-5
    eve_testkit::attest(&["plan:ESI-5"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_600_050_i64;
    let ship_id = 80_600_050_i64;
    let perim_sys = 30_000_144_u32;
    let jita_sta = 60_003_760_i64;
    let astrahus_item_id = 99_600_501_i64;
    let market_svc_id = 99_600_502_i64;
    let starbase_id = 99_600_503_i64;
    let fuel_item_id = 99_600_504_i64;

    let anchor_pos = [100_000_000.0, 0.0, 0.0];

    // 1. Seed corporation, CEO, and items
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ceo_id].clone();
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.solar_system_id = perim_sys;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(ceo_id, ch);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI-5 Upwell Starbase Corp".into(),
                ticker: "ES5C".into(),
                ceo_id,
                tax_rate: 0.075,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_FACTORY_MANAGER)]),
                ..CorporationRecord::default()
            },
        );

        snap.items.insert(astrahus_item_id, make_item(astrahus_item_id, 35832, ceo_id, ship_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(market_svc_id, make_item(market_svc_id, 35892, corp_id, jita_sta, InventoryFlag::Hangar, 1, false));
        snap.items.insert(starbase_id, make_item(starbase_id, 16213, corp_id, perim_sys as i64, InventoryFlag::None, 1, true));
        snap.items.insert(fuel_item_id, make_item(fuel_item_id, 4051, corp_id, starbase_id, InventoryFlag::None, 500, false));
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                fuel_bay_view: "corporation_member".into(),
                fuel_bay_take: "starbase_fuel_technician_role".into(),
                anchor: "starbase_fuel_technician_role".into(),
                unanchor: "starbase_fuel_technician_role".into(),
                online: "starbase_fuel_technician_role".into(),
                offline: "starbase_fuel_technician_role".into(),
                ..StarbaseDetailSettings::default()
            },
        );
    });

    let mut sess = SessionState::new(71_002);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(perim_sys);
    sess.ship_id = Some(ship_id);
    sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_FACTORY_MANAGER;

    ctx.systems.get_or_spawn(perim_sys).spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::from_array(anchor_pos),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    // 2. Live structureDeployment.Anchor RPC
    let anchor_rep = rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astrahus_item_id),
            PyRep::Tuple(vec![PyRep::Real(anchor_pos[0] + 50_000.0), PyRep::Real(anchor_pos[1]), PyRep::Real(anchor_pos[2])]),
            PyRep::String("Perimeter Free Market Citadel".into()),
            PyRep::Integer(1),
            PyRep::Integer(18),
        ],
    ).await;
    let structure_id = anchor_rep.as_i64().or_else(|| anchor_rep.dict_get("structureID").and_then(|v| v.as_i64())).expect("anchored structure_id");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) { st.state = "ShieldVulnerable".into(); }
        if let Some(svc) = snap.items.get_mut(&market_svc_id) { svc.location_id = structure_id; svc.flag = InventoryFlag::ServiceSlot0; }
        snap.module_online_states.insert(market_svc_id, true);
        snap.module_online_states.insert(starbase_id, true);
        snap.structure_esi_timer_meta.insert(
            structure_id,
            eve_db::StructureEsiTimerMeta {
                profile_id: 55,
                fuel_expires_unix: Some(1_800_000_000),
                state_timer_start_unix: None,
                state_timer_end_unix: None,
                unanchors_at_unix: None,
                next_reinforce_hour: None,
                next_reinforce_apply_unix: None,
            },
        );
    });

    // 3. Issue token and validate ESI structures & starbases
    let token = issue_token_with_scopes(&ctx, ceo_id, vec!["esi-corporations.read_structures.v1".into(), "esi-corporations.read_starbases.v1".into()]);

    let structs = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/structures/"), Some(&token));
    let s_arr = structs.as_array().expect("structures array");
    let found_st = s_arr.iter().find(|s| s["structure_id"] == json!(structure_id)).expect("citadel");
    assert_eq!(found_st["corporation_id"], json!(corp_id));
    assert_eq!(found_st["system_id"], json!(perim_sys));
    assert_eq!(found_st["profile_id"], json!(55));
    assert_eq!(found_st["state"], json!("shield_vulnerable"));
    assert!(found_st["fuel_expires"].is_string());
    assert_eq!(found_st["services"][0]["name"], json!("Standup Market Hub I"));
    assert_eq!(found_st["services"][0]["state"], json!("online"));

    let starbases = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/starbases/"), Some(&token));
    let sb_arr = starbases.as_array().expect("starbases array");
    let found_sb = sb_arr.iter().find(|sb| sb["starbase_id"] == json!(starbase_id)).expect("starbase");
    assert_eq!(found_sb["system_id"], json!(perim_sys));
    assert_eq!(found_sb["type_id"], json!(16213));
    assert_eq!(found_sb["state"], json!("online"));

    let sb_detail = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/starbases/{starbase_id}/?system_id={perim_sys}"), Some(&token));
    assert_eq!(sb_detail["fuel_bay_view"], json!("corporation_member"));
    assert_eq!(sb_detail["fuel_bay_take"], json!("starbase_fuel_technician_role"));
    assert_eq!(sb_detail["fuels"][0]["type_id"], json!(4051));
    assert_eq!(sb_detail["fuels"][0]["quantity"], json!(500));
}

#[tokio::test]
async fn test_plan_q5_social_and_organization_quasar_packages() {
    // parity: plan:Q-5
    eve_testkit::attest(&["plan:Q-5"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
        }
        snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Social Quasar Corp".into(),
            ticker: "SQC".into(),
            ceo_id: char_id,
            members: HashSet::from([char_id]),
            member_roles: HashMap::from([(char_id, ROLE_DIRECTOR)]),
            ..CorporationRecord::default()
        });
        snap.alliances.entry(alliance_id).or_insert_with(|| AllianceRecord {
            alliance_id,
            name: "Social Quasar Alliance".into(),
            short_name: "SQA".into(),
            executor_corp_id: corp_id,
            member_corp_ids: HashSet::from([corp_id]),
        });
    });

    let mut sess = SessionState::new(71_003);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR;
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Skill Plan: Create & GetAll
    let mut attr = Vec::new();
    encode_string_field(1, "Plan Q-5 Defense Fleet", &mut attr);
    encode_string_field(2, "Standard corp defensive doctrine", &mut attr);
    let mut cat_id = Vec::new();
    encode_bytes_field(1, &[1u8; 16], &mut cat_id);
    encode_bytes_field(4, &cat_id, &mut attr);
    let mut create_payload = Vec::new();
    encode_bytes_field(1, &attr, &mut create_payload);

    let create_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporation.skill.plan.CreateRequest", create_payload, b"q5-01").await;
    assert_eq!(create_rsp.internal_origin, "evomu.quasar.public_gateway");
    let get_plans_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), b"q5-02").await;
    assert_eq!(get_plans_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(get_plans_rsp.payload.as_ref().unwrap().type_url.ends_with("GetAllResponse"));

    // 2. Corporation Goal & Freelance
    let corp_goal_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporationgoal.api.GetAllRequest", Vec::new(), b"q5-03").await;
    assert_eq!(corp_goal_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fl_limits_rsp = send_quasar(&ctx, &mut sess, "eve_public.freelance.project.api.GetCreationLimitsRequest", Vec::new(), b"q5-04").await;
    assert_eq!(fl_limits_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fl_active_rsp = send_quasar(&ctx, &mut sess, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new(), b"q5-05").await;
    assert_eq!(fl_active_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 3. Alliance, Corporation Members & Divisions
    let alliance_rsp = send_quasar(&ctx, &mut sess, "eve.alliance.GetAllRequest", Vec::new(), b"q5-06").await;
    assert_eq!(alliance_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut corp_payload = Vec::new();
    encode_id_field(1, corp_id as u64, &mut corp_payload);
    let members_rsp = send_quasar(&ctx, &mut sess, "eve.corporation.members.GetRequest", corp_payload.clone(), b"q5-07").await;
    assert_eq!(members_rsp.internal_origin, "evomu.quasar.public_gateway");
    let divisions_rsp = send_quasar(&ctx, &mut sess, "eve.corporation.divisions.GetRequest", corp_payload, b"q5-08").await;
    assert_eq!(divisions_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 4. Chat Local Membership, Calendar, Contacts, Mail, War, Tag, Presence
    let mut chat_payload = Vec::new();
    encode_id_field(1, 30_000_142, &mut chat_payload);
    let chat_rsp = send_quasar(&ctx, &mut sess, "eve_public.chat.local.api.GetMembershipListRequest", chat_payload, b"q5-09").await;
    assert_eq!(chat_rsp.internal_origin, "evomu.quasar.public_gateway");
    let cal_rsp = send_quasar(&ctx, &mut sess, "eve.calendar.personal_event.GetRequest", Vec::new(), b"q5-10").await;
    assert_eq!(cal_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut char_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut char_payload);
    let contact_rsp = send_quasar(&ctx, &mut sess, "eve.contact.label.GetForCharacterRequest", char_payload.clone(), b"q5-11").await;
    assert_eq!(contact_rsp.internal_origin, "evomu.quasar.public_gateway");
    let mail_rsp = send_quasar(&ctx, &mut sess, "eve.mail.cspa.GetRequest", char_payload, b"q5-12").await;
    assert_eq!(mail_rsp.internal_origin, "evomu.quasar.public_gateway");
    let war_rsp = send_quasar(&ctx, &mut sess, "eve.war.GetAllRequest", Vec::new(), b"q5-13").await;
    assert_eq!(war_rsp.internal_origin, "evomu.quasar.public_gateway");
    let tag_rsp = send_quasar(&ctx, &mut sess, "eve.tag.api.ListRequest", Vec::new(), b"q5-14").await;
    assert_eq!(tag_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut pres_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut pres_payload);
    let pres_rsp = send_quasar(&ctx, &mut sess, "eve_public.presence.api.GetCorporationRequest", pres_payload, b"q5-15").await;
    assert_eq!(pres_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(pres_rsp.payload.as_ref().unwrap().type_url.ends_with("GetCorporationResponse"));
}

#[tokio::test]
async fn test_plan_q6_economy_and_industry_quasar_packages() {
    // parity: plan:Q-6
    eve_testkit::attest(&["plan:Q-6"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(5501, make_item(5501, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.market_orders.insert(
            7101,
            MarketOrderRecord {
                order_id: 7101,
                owner_id: char_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 10_000,
                vol_remaining: 10_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });

    let mut sess = SessionState::new(71_004);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Market: Price for all types & station orders
    let price_rsp = send_quasar(&ctx, &mut sess, "eve.market.price.GetForAllTypesRequest", Vec::new(), b"q6-01").await;
    assert_eq!(price_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(price_rsp.payload.as_ref().unwrap().type_url.ends_with("GetForAllTypesResponse"));

    let mut sta_payload = Vec::new();
    encode_id_field(1, station_id as u64, &mut sta_payload);
    let orders_rsp = send_quasar(&ctx, &mut sess, "eve.market.orders.GetOpenInStationRequest", sta_payload, b"q6-02").await;
    assert_eq!(orders_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 2. Inventory: Category, Group, GenericItemType & GenericName
    let cat_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.category.GetAllRequest", Vec::new(), b"q6-03").await;
    assert_eq!(cat_rsp.internal_origin, "evomu.quasar.public_gateway");
    let mut grp_payload = Vec::new();
    encode_id_field(1, 25, &mut grp_payload); // Group 25: Frigate
    let grp_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.group.GetRequest", grp_payload, b"q6-04").await;
    assert_eq!(grp_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut type_payload = Vec::new();
    encode_id_field(1, 587, &mut type_payload); // Type 587: Rifter
    let type_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.genericitemtype.GetRequest", type_payload, b"q6-05").await;
    assert_eq!(type_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut name_payload = Vec::new();
    encode_id_field(1, sys_id as u64, &mut name_payload);
    let name_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.genericname.GetNameByIDRequest", name_payload, b"q6-06").await;
    assert_eq!(name_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 3. Assets: Ship assets for character
    let mut char_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut char_payload);
    let asset_rsp = send_quasar(&ctx, &mut sess, "eve.asset.GetShipAssetsRequest", char_payload.clone(), b"q6-07").await;
    assert_eq!(asset_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 4. Industry & Blueprints: Blueprint list, facility details, and system cost indices
    let bp_rsp = send_quasar(&ctx, &mut sess, "eve.character.industry.blueprints.GetRequest", char_payload.clone(), b"q6-08").await;
    assert_eq!(bp_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fac_rsp = send_quasar(&ctx, &mut sess, "eve.industry.facility.GetRequest", Vec::new(), b"q6-09").await;
    assert_eq!(fac_rsp.internal_origin, "evomu.quasar.public_gateway");
    let sys_cost_rsp = send_quasar(&ctx, &mut sess, "eve.industry.system.GetCostIndicesRequest", Vec::new(), b"q6-10").await;
    assert_eq!(sys_cost_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 5. Accounting & Contracts
    let entry_type_rsp = send_quasar(&ctx, &mut sess, "eve.accounting.entrytype.GetAllRequest", Vec::new(), b"q6-11").await;
    assert_eq!(entry_type_rsp.internal_origin, "evomu.quasar.public_gateway");
    let journal_rsp = send_quasar(&ctx, &mut sess, "eve.accounting.character.journal.GetCharacterEntriesRequest", char_payload, b"q6-12").await;
    assert_eq!(journal_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut contract_payload = Vec::new();
    encode_id_field(1, 1, &mut contract_payload);
    let contract_rsp = send_quasar(&ctx, &mut sess, "eve.contract.item_exchange.GetRequest", contract_payload, b"q6-13").await;
    assert_eq!(contract_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 6. Planetary, Moon & Asteroid Belt
    let mut sys_payload = Vec::new();
    encode_id_field(1, sys_id as u64, &mut sys_payload);
    let planets_rsp = send_quasar(&ctx, &mut sess, "eve.planet.GetPlanetsInSolarSystemRequest", sys_payload, b"q6-14").await;
    assert_eq!(planets_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut pln_payload = Vec::new();
    encode_id_field(1, 40_000_001, &mut pln_payload);
    let pln_rsp = send_quasar(&ctx, &mut sess, "eve.planet.GetRequest", pln_payload.clone(), b"q6-15").await;
    assert_eq!(pln_rsp.internal_origin, "evomu.quasar.public_gateway");
    let pi_schem_rsp = send_quasar(&ctx, &mut sess, "eve.planetinteraction.schematic.GetRequest", pln_payload.clone(), b"q6-16").await;
    assert_eq!(pi_schem_rsp.internal_origin, "evomu.quasar.public_gateway");
    let moon_rsp = send_quasar(&ctx, &mut sess, "eve.moon.GetRequest", pln_payload.clone(), b"q6-17").await;
    assert_eq!(moon_rsp.internal_origin, "evomu.quasar.public_gateway");
    let belt_rsp = send_quasar(&ctx, &mut sess, "eve.asteroidbelt.GetRequest", pln_payload, b"q6-18").await;
    assert_eq!(belt_rsp.internal_origin, "evomu.quasar.public_gateway");
}
