//! End-to-end remediation integration tests for ESI social, calendar, contacts,
//! mailing lists, corporation governance, and sovereignty map (`esi:*` fake_done closure).

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{issue_token_with_scopes, rpc_call};
use eve_db::corp_roles::{
    ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_PROJECT_MANAGER, ROLE_TRADER,
};
use eve_db::{AllianceRecord, CorporationRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::claim_system_sovereignty;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

fn esi_put_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str, body: &Value) {
    let req = EsiHttpRequest::new("PUT", path)
        .with_bearer_token(token)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_vec(body).unwrap());
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 204, "PUT {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    spec.validate_response("PUT", path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for PUT {path}: {e}"));
}

#[tokio::test]
async fn test_parity_remediation_esi_calendar_rpc_and_http_persistence() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}, esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let jita_station = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Calendar CEO"), (wingman_id, "Calendar Wingman")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Calendar Corp".into(),
                ticker: "CALC".into(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, wingman_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR), (wingman_id, 0)]),
                ..CorporationRecord::default()
            },
        );
    });

    let mut ceo_sess = SessionState::new(70_101);
    ceo_sess.user_id = Some(1);
    ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.station_id = Some(jita_station);
    ceo_sess.solar_system_id = Some(30_000_142);

    let mut wing_sess = SessionState::new(70_102);
    wing_sess.user_id = Some(1);
    wing_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    wing_sess.character_id = Some(wingman_id);
    wing_sess.corporation_id = Some(corp_id);
    wing_sess.station_id = Some(jita_station);
    wing_sess.solar_system_id = Some(30_000_142);

    let event_ft = ((ctx.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
    let create_rep = rpc_call(
        &ctx,
        &mut ceo_sess,
        "calendarMgr",
        "CreateCorporationEvent",
        &[
            PyRep::Integer(event_ft),
            PyRep::Integer(90),
            PyRep::String("Fleet Stratop".into()),
            PyRep::String("Form up in Jita 4-4".into()),
            PyRep::Integer(1),
        ],
    )
    .await;
    let event_id = create_rep
        .as_i64()
        .or_else(|| create_rep.dict_get("eventID").and_then(|v| v.as_i64()))
        .expect("created corporation event_id");
    assert!(event_id > 0);

    // Respond via MachoNet RPC as wingman (5 = tentative)
    rpc_call(
        &ctx,
        &mut wing_sess,
        "calendarMgr",
        "SendEventResponse",
        &[
            PyRep::Integer(event_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(5),
        ],
    )
    .await;

    // Respond via ESI HTTP PUT as CEO ("accepted")
    let cal_tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-calendar.read_calendar_events.v1",
            "esi-calendar.respond_calendar_events.v1",
        ],
    );
    esi_put_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
        &cal_tok,
        &json!({ "response": "accepted" }),
    );

    // Clear ephemeral esi_state to prove ESI reads directly from WorldStateSnapshot
    ctx.esi_state.calendar_events.write().unwrap().clear();
    ctx.esi_state.calendar_responses.write().unwrap().clear();

    let list = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        Some(&cal_tok),
    );
    let arr = list.as_array().expect("calendar list array");
    let item = arr.iter().find(|e| e["event_id"] == json!(event_id)).expect("event in calendar list");
    assert_eq!(item["title"], json!("Fleet Stratop"));
    assert_eq!(item["importance"], json!(1));
    assert_eq!(item["event_response"], json!("accepted"));

    let detail = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
        Some(&cal_tok),
    );
    assert_eq!(detail["event_id"], json!(event_id));
    assert_eq!(detail["owner_id"], json!(corp_id));
    assert_eq!(detail["owner_type"], json!("corporation"));
    assert_eq!(detail["duration"], json!(90));
    assert_eq!(detail["text"], json!("Form up in Jita 4-4"));
    assert_eq!(detail["response"], json!("accepted"));

    let attendees = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/attendees/"),
        Some(&cal_tok),
    );
    let att_arr = attendees.as_array().expect("attendees array");
    assert!(att_arr.iter().any(|a| a["character_id"] == json!(ceo_id) && a["event_response"] == json!("accepted")));
    assert!(att_arr.iter().any(|a| a["character_id"] == json!(wingman_id) && a["event_response"] == json!("tentative")));
}

#[tokio::test]
async fn test_parity_remediation_esi_contacts_labels_and_mail_lists() {
    // parity: esi:GET /characters/{character_id}/contacts/labels, esi:GET /corporations/{corporation_id}/contacts, esi:GET /corporations/{corporation_id}/contacts/labels, esi:GET /alliances/{alliance_id}/contacts, esi:GET /alliances/{alliance_id}/contacts/labels, esi:GET /characters/{character_id}/mail/lists
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts/labels",
        "esi:GET /corporations/{corporation_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "esi:GET /characters/{character_id}/mail/lists",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let target_char_id = 90_000_002_i64;
    let corp_id = 98_600_002_i64;
    let alliance_id = 99_600_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Diplo CEO"), (target_char_id, "Neutral Contact")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = if cid == ceo_id { corp_id } else { 1_000_009 };
            ch.alliance_id = if cid == ceo_id { Some(alliance_id) } else { None };
            ch.balance = IskCents::from_isk(50_000_000);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Diplo Corp".into(),
                ticker: "DPLC".into(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR)]),
                ..CorporationRecord::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Diplo Alliance".into(),
                short_name: "DPLA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    let mut sess = SessionState::new(70_103);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.corp_role = ROLE_DIRECTOR;

    // 1. Character contact label via charMgr.CreateLabel
    rpc_call(
        &ctx,
        &mut sess,
        "charMgr",
        "CreateLabel",
        &[PyRep::String("Logi Anchors".into()), PyRep::Integer(1)],
    )
    .await;

    // 2. Corporation contact & label via corpRegistry
    rpc_call(
        &ctx,
        &mut sess,
        "corpRegistry",
        "CreateLabel",
        &[PyRep::String("Blues".into()), PyRep::Integer(2)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(target_char_id), PyRep::Real(5.0), PyRep::Integer(1)],
    )
    .await;

    // 3. Alliance contact & label via allianceRegistry
    rpc_call(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "CreateLabel",
        &[PyRep::String("Coalition".into()), PyRep::Integer(3)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(target_char_id), PyRep::Real(10.0), PyRep::Integer(1)],
    )
    .await;

    // 4. Mailing list via mailingListsMgr.Create
    rpc_call(
        &ctx,
        &mut sess,
        "mailingListsMgr",
        "Create",
        &[
            PyRep::String("FC Announcements".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await;

    // Clear ephemeral esi_state maps to prove SQLite/WorldStateSnapshot backing
    ctx.esi_state.char_contact_labels.write().unwrap().clear();
    ctx.esi_state.corp_contacts.write().unwrap().clear();
    ctx.esi_state.corp_contact_labels.write().unwrap().clear();
    ctx.esi_state.alliance_contacts.write().unwrap().clear();
    ctx.esi_state.alliance_contact_labels.write().unwrap().clear();
    ctx.esi_state.mail_lists.write().unwrap().clear();

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-characters.read_contacts.v1",
            "esi-corporations.read_contacts.v1",
            "esi-alliances.read_contacts.v1",
            "esi-mail.read_mail.v1",
        ],
    );

    let char_labels = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&tok));
    assert!(char_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Logi Anchors")));

    let corp_contacts = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/"), Some(&tok));
    assert!(corp_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(5.0)));

    let corp_labels = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/labels/"), Some(&tok));
    assert!(corp_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Blues")));

    let ally_contacts = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/"), Some(&tok));
    assert!(ally_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(10.0)));

    let ally_labels = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/labels/"), Some(&tok));
    assert!(ally_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Coalition")));

    let mail_lists = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/mail/lists/"), Some(&tok));
    assert!(mail_lists.as_array().unwrap().iter().any(|m| m["name"] == json!("FC Announcements")));
}

#[tokio::test]
async fn test_parity_remediation_esi_corp_governance_roles_limit_and_sov_map() {
    // parity: esi:GET /corporations/{corporation_id}/members/limit, esi:GET /corporations/{corporation_id}/roles, esi:GET /corporations/{corporation_id}/roles/history, esi:GET /sovereignty/map
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members/limit",
        "esi:GET /corporations/{corporation_id}/roles",
        "esi:GET /corporations/{corporation_id}/roles/history",
        "esi:GET /sovereignty/map",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let corp_id = 98_600_003_i64;
    let alliance_id = 99_600_003_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .expect("SDE nullsec solar system");

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Gov CEO"), (member_id, "Gov Member")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            snap.characters.insert(cid, ch);
        }
        snap.skills.entry(ceo_id).or_default().clear();
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Governance Corp".into(),
                ticker: "GOVC".into(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, member_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR), (member_id, 0)]),
                ..CorporationRecord::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Governance Alliance".into(),
                short_name: "GOVA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1",
            "esi-corporations.read_titles.v1",
            "esi-corporations.track_members.v1",
        ],
    );

    // 1. Untrained CEO defaults to base 10 member limit; training Corporation Management + UpdateCorporationAbilities updates it
    let base_limit = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/members/limit/"),
        Some(&tok),
    );
    assert_eq!(base_limit, json!(10));

    let sde_bonus = ctx
        .sde
        .types
        .get(&3363)
        .and_then(|t| t.default_attributes.get(&191))
        .copied()
        .expect("SDE attribute 191 on Corporation Management (3363)")
        .round() as i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(ceo_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 3,
                skill_points: 16_000.0,
            },
        );
    });

    let mut ceo_sess = SessionState::new(70_104);
    ceo_sess.user_id = Some(1);
    ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.alliance_id = Some(alliance_id);
    ceo_sess.corp_role = ROLE_DIRECTOR;

    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporationAbilities",
        &[],
    )
    .await;

    let updated_limit = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/members/limit/"),
        Some(&tok),
    );
    assert_eq!(updated_limit, json!(sde_bonus * 3));

    // 2. Live corpRegistry.UpdateMember grants Trader, Contract_Manager, Project_Manager -> reflected in /roles and /roles/history
    let granted_mask = (ROLE_TRADER | ROLE_CONTRACT_MANAGER | ROLE_PROJECT_MANAGER) as i64;
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(member_id),
            PyRep::String(String::new()),
            PyRep::Integer(granted_mask),
            PyRep::Integer(0),
        ],
    )
    .await;
    ctx.advance_secs(86_405);
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(member_id),
            PyRep::String(String::new()),
            PyRep::Integer(granted_mask),
            PyRep::Integer(0),
        ],
    )
    .await;
    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1",
            "esi-corporations.read_titles.v1",
            "esi-corporations.track_members.v1",
        ],
    );

    let roles_json = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/roles/"),
        Some(&tok),
    );
    let member_entry = roles_json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["character_id"] == json!(member_id))
        .expect("member in /roles");
    let role_names: Vec<&str> = member_entry["roles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(role_names.contains(&"Trader"));
    assert!(role_names.contains(&"Contract_Manager"));
    assert!(role_names.contains(&"Project_Manager"));

    let history_json = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/roles/history/"),
        Some(&tok),
    );
    let hist_entry = history_json
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["character_id"] == json!(member_id) && h["issuer_id"] == json!(ceo_id))
        .expect("role history entry");
    let new_roles: Vec<&str> = hist_entry["new_roles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(new_roles.contains(&"Trader"));
    assert!(new_roles.contains(&"Contract_Manager"));

    // 3. Live sovereignty claim -> reflected in GET /sovereignty/map
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
    });
    claim_system_sovereignty(&ctx, null_sys, alliance_id, corp_id, 0)
        .expect("claim_system_sovereignty");
    let sov_map = esi_get_validated(&router, &spec, "/v1/sovereignty/map/", None);
    let sys_entry = sov_map
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["system_id"] == json!(null_sys as i64))
        .expect("claimed system in /sovereignty/map");
    assert_eq!(sys_entry["alliance_id"], json!(alliance_id));
    assert_eq!(sys_entry["corporation_id"], json!(corp_id));
}
