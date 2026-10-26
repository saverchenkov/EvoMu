//! Integration tests for Skill Parity features:
//! - `feat:skills.alpha-skill-caps`
//! - `feat:skills.skill-history`
//! - `feat:skills.accelerators`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{NeuralAttributes, SkillHistoryEntry, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::router::ServerContext;

use crate::common;

async fn login_and_select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
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
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"skillplan-test-1".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-test".to_vec(),
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

/// Parity test: `feat:skills.alpha-skill-caps`
/// Alpha characters cannot train above the alpha cap per skill; SP above cap is inactive.
#[tokio::test]
async fn test_feat_skills_alpha_skill_caps() {
    // parity: feat:skills.alpha-skill-caps
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;
    let char_id = 90_000_001;

    // 1. Configure character as Alpha (clone_grade = 1)
    // Skill 3402 (Science) has Alpha cap 4 in clonegrades.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 100_000.0;
        ch.free_skill_points = 1_000.0;
        let skills = snap.skills.entry(char_id).or_default();
        // Spaceship Command (3300) level 1 (250 SP)
        skills.insert(3300, TrainedSkillRecord { type_id: 3300, level: 1, skill_points: 250.0 });
        // Science (3402) level 1 (250 SP)
        skills.insert(3402, TrainedSkillRecord { type_id: 3402, level: 1, skill_points: 250.0 });
    });

    // 2. Queueing skill 3402 to level 5 must fail with AlphaCloneSkillLevelCapExceeded
    let queue_over_cap = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(3402), PyRep::Integer(5)]),
    ]);
    let err = rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[queue_over_cap]).await.unwrap_err();
    assert_eq!(err.code, "AlphaCloneSkillLevelCapExceeded");

    // Give Science (3402) level 5: while Alpha, effective level is clamped to cap 4 (SP above cap inactive)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3402, TrainedSkillRecord { type_id: 3402, level: 5, skill_points: 256_000.0 });
    });
    let effective = ctx.character_skill_levels(char_id);
    assert_eq!(effective.get(&3402).copied(), Some(4), "Alpha clone SP above cap must be inactive");

    // 3. Configure total SP at 5,000,000 limit -> SaveSkillQueue fails with AlphaCloneSpCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.skill_points = 4_999_000.0;
    });
    let queue_arg = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(2)]),
    ]);
    let err = rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[queue_arg.clone()]).await.unwrap_err();
    assert_eq!(err.code, "AlphaCloneSpCapExceeded");

    // 4. Free SP can still be applied directly to known skills (up to Alpha skill caps)
    let res = rpc(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Real(500.0)]).await;
    assert!(res.is_ok(), "Applying free SP must succeed even when total SP >= 5M: {res:?}");
    ctx.db.with_snapshot(|snap| {
        let skill = snap.skills[&char_id].get(&3300).unwrap();
        assert_eq!(skill.skill_points, 750.0);
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.free_skill_points, 500.0);
    });

    // 5. Upgrade character to Omega (clone_grade = 2): full level 5 active and queueing succeeds
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
    });
    let omega_effective = ctx.character_skill_levels(char_id);
    assert_eq!(omega_effective.get(&3402).copied(), Some(5), "Omega clone restores full active skill level");

    let ok = rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[queue_arg]).await;
    assert!(ok.is_ok(), "Omega accounts remove Alpha skill caps: {ok:?}");
}

/// Parity test: `feat:skills.skill-history`
/// GetSkillHistory returns chronological entries for injection (36), completion (37), removal (38), extraction (39).
/// Obsolete skills are automatically migrated and refunded during logon (SelectCharacterID).
#[tokio::test]
async fn test_feat_skills_history_events_and_obsolete_refund() {
    // parity: feat:skills.skill-history
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 1004, "demo", "demo-secret", 90_000_001).await;
    let char_id = 90_000_001;

    // 1. Seed skill history entries for all 4 types: injection (36), training (37), removal (38), extraction (39)
    let t0 = 133_000_000_000_000_000_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let hist = snap.skill_history.entry(char_id).or_default();
        hist.clear();
        hist.push(SkillHistoryEntry { log_date_ft: t0 + 200, event_type_id: 37, skill_type_id: 3300, skill_level: 2, absolute_points: 1414.0 });
        hist.push(SkillHistoryEntry { log_date_ft: t0 + 100, event_type_id: 36, skill_type_id: 3300, skill_level: 0, absolute_points: 0.0 });
        hist.push(SkillHistoryEntry { log_date_ft: t0 + 400, event_type_id: 39, skill_type_id: 3300, skill_level: 1, absolute_points: 250.0 });
        hist.push(SkillHistoryEntry { log_date_ft: t0 + 300, event_type_id: 38, skill_type_id: 3300, skill_level: 0, absolute_points: 0.0 });
    });

    // Call GetSkillHistory: must return chronological order (by log_date)
    let rep = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillHistory", &[]).await.expect("GetSkillHistory");
    let PyRep::Rowset { columns: _, rows } = rep else { panic!("Expected Rowset, got {rep:?}") };
    assert_eq!(rows.len(), 4);
    let event_ids: Vec<i64> = rows.iter().map(|cols| {
        cols.get(1).and_then(|v| v.as_i64()).unwrap()
    }).collect();
    assert_eq!(event_ids, vec![36, 37, 38, 39], "GetSkillHistory must return rows in chronological order");

    // 2. Test obsolete skill migration on logon: pilot has obsolete skill 999_999 with 1,500 trained SP.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.skill_points = 50_000.0;
        ch.free_skill_points = 2_000.0;
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(999_999, TrainedSkillRecord { type_id: 999_999, level: 2, skill_points: 1_500.0 });
    });

    // SelectCharacterID logon flow triggers obsolete skill migration
    let sel_res = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    assert!(sel_res.is_ok(), "SelectCharacterID must succeed: {sel_res:?}");

    // Skill removed from trained skills, SP deducted from total and credited to unallocated free_skill_points
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.skills[&char_id].contains_key(&999_999));
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.skill_points, 48_500.0);
        assert_eq!(ch.free_skill_points, 3_500.0);
        // History event 38 logged
        let last_ev = snap.skill_history[&char_id].last().unwrap();
        assert_eq!(last_ev.event_type_id, 38);
        assert_eq!(last_ev.skill_type_id, 999_999);
    });
}

/// Parity test: `feat:skills.accelerators`
/// Also tests certificates and Quasar skill plans.
#[tokio::test]
async fn test_feat_skills_certificates_and_plans_and_accelerators() {
    // parity: feat:skills.accelerators
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 1005, "demo", "demo-secret", 90_000_001).await;
    let char_id = 90_000_001;

    // --- Certificates (`feat:skills.certificates`) ---
    // 1. GetCertificateCategories and GetCertificateClasses
    // rpc-lint: allow crucible legacy service certificateMgr
    let cat_rep = rpc(&ctx, &mut sess, "certificateMgr", "GetCertificateCategories", &[]).await.expect("GetCertificateCategories");
    let PyRep::List(categories) = cat_rep else { panic!("expected List") };
    assert!(!categories.is_empty(), "GetCertificateCategories must return categories");

    // rpc-lint: allow crucible legacy service certificateMgr
    let class_rep = rpc(&ctx, &mut sess, "certificateMgr", "GetCertificateClasses", &[]).await.expect("GetCertificateClasses");
    let PyRep::List(classes) = class_rep else { panic!("expected List") };
    assert!(!classes.is_empty(), "GetCertificateClasses must return classes");

    // 1b. Certificate derivation from trained skills (feat:skills.certificates acceptance)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3300, TrainedSkillRecord { type_id: 3300, level: 4, skill_points: 45_255.0 });
    });
    // rpc-lint: allow crucible legacy service certificateMgr
    let derived_certs = rpc(&ctx, &mut sess, "certificateMgr", "GetMyCertificates", &[]).await.expect("GetMyCertificates derived");
    let PyRep::KeyVal(dm) = derived_certs else { panic!("expected KeyVal") };
    let PyRep::Dict(d_list) = dm.get("certificates").unwrap() else { panic!("expected Dict") };
    assert!(!d_list.is_empty(), "GetMyCertificates must report derived certificate levels for trained skills");
    assert!(d_list.iter().any(|(_, lvl)| lvl.as_i64().unwrap_or(0) >= 2), "Certificate level must be derived from skill level");

    // 2. Grant individual certificate
    // rpc-lint: allow crucible legacy service certificateMgr
    let grant_rep = rpc(&ctx, &mut sess, "certificateMgr", "GrantCertificate", &[
        PyRep::Integer(char_id),
        PyRep::Integer(101),
        PyRep::Integer(3),
    ]).await.expect("GrantCertificate");
    assert_eq!(grant_rep.as_bool(), Some(true));

    // 3. BatchCertificateGrant: grants multiple certificates
    // rpc-lint: allow crucible legacy service certificateMgr
    let batch_grant = rpc(&ctx, &mut sess, "certificateMgr", "BatchCertificateGrant", &[
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(201), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(202), PyRep::Integer(4)]),
        ])
    ]).await.expect("BatchCertificateGrant");
    assert_eq!(batch_grant.as_bool(), Some(true));

    // 4. UpdateCertificateFlags: updates flags
    // rpc-lint: allow crucible legacy service certificateMgr
    let flag_rep = rpc(&ctx, &mut sess, "certificateMgr", "UpdateCertificateFlags", &[
        PyRep::Integer(201),
        PyRep::Integer(7),
    ]).await.expect("UpdateCertificateFlags");
    assert_eq!(flag_rep.as_bool(), Some(true));

    // 5. BatchCertificateUpdate: updates level and flags
    // rpc-lint: allow crucible legacy service certificateMgr
    let batch_upd = rpc(&ctx, &mut sess, "certificateMgr", "BatchCertificateUpdate", &[
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(202), PyRep::Integer(5), PyRep::Integer(3)]),
        ])
    ]).await.expect("BatchCertificateUpdate");
    assert_eq!(batch_upd.as_bool(), Some(true));

    // 6. Verify stateful persistence in snap.service_state_blobs["character_certificates:{char_id}"]
    ctx.db.with_snapshot(|snap| {
        let blob_key = format!("character_certificates:{char_id}");
        let blob = snap.service_state_blobs.get(&blob_key).expect("state blob must exist");
        let parsed: serde_json::Value = serde_json::from_str(blob).expect("valid json blob");
        assert_eq!(parsed["certificates"]["201"], 2);
        assert_eq!(parsed["certificates"]["202"], 5);
        assert_eq!(parsed["flags"]["201"], 7);
        assert_eq!(parsed["flags"]["202"], 3);
    });

    // 7. GetMyCertificates: reports derived and explicitly granted levels and flags
    // rpc-lint: allow crucible legacy service certificateMgr
    let my_certs = rpc(&ctx, &mut sess, "certificateMgr", "GetMyCertificates", &[]).await.expect("GetMyCertificates");
    let PyRep::KeyVal(m) = my_certs else { panic!("expected KeyVal") };
    let PyRep::Dict(cert_list) = m.get("certificates").unwrap() else { panic!("expected Dict") };
    let has_cert_101 = cert_list.iter().any(|(c, lvl)| c.as_i64() == Some(101) && lvl.as_i64() == Some(3));
    let has_cert_202 = cert_list.iter().any(|(c, lvl)| c.as_i64() == Some(202) && lvl.as_i64() == Some(5));
    assert!(has_cert_101 && has_cert_202, "GetMyCertificates must report all granted certificates");

    let PyRep::Dict(flag_list) = m.get("certificateFlags").unwrap() else { panic!("expected Dict") };
    let has_flag_201 = flag_list.iter().any(|(c, flg)| c.as_i64() == Some(201) && flg.as_i64() == Some(7));
    assert!(has_flag_201, "GetMyCertificates must report certificate flags");

    // rpc-lint: allow crucible legacy service certificateMgr
    let recs = rpc(&ctx, &mut sess, "certificateMgr", "GetAllShipCertificateRecommendations", &[]).await.expect("GetAllShipCertificateRecommendations");
    assert!(matches!(recs, PyRep::KeyVal(_)));

    // 8. Auth gate: mutating another character's certificates without GM permissions is denied
    let other_char_id = 90_000_002;
    // rpc-lint: allow crucible legacy service certificateMgr
    let err_unauth = rpc(&ctx, &mut sess, "certificateMgr", "GrantCertificate", &[
        PyRep::Integer(other_char_id),
        PyRep::Integer(101),
        PyRep::Integer(5),
    ]).await.unwrap_err();
    assert_eq!(err_unauth.code, "AccessDenied");

    // With GM permissions (ROLE_GML), mutating another character's certificates succeeds
    sess.role |= eve_machonet::service_roles::ROLE_GML;
    // rpc-lint: allow crucible legacy service certificateMgr
    let gm_grant = rpc(&ctx, &mut sess, "certificateMgr", "GrantCertificate", &[
        PyRep::Integer(other_char_id),
        PyRep::Integer(101),
        PyRep::Integer(5),
    ]).await.expect("GM GrantCertificate for other character");
    assert_eq!(gm_grant.as_bool(), Some(true));

    // --- Skill Plans (`feat:skills.skill-plans`) ---
    // Skill plans in live V24 are managed through the public gateway via Quasar (quasar:skill)
    let (mut sp_attr, mut sp_req) = (Vec::new(), Vec::new());
    encode_string_field(1, "Frigate Mastery", &mut sp_attr);
    encode_string_field(3, "Core combat frigate skills", &mut sp_attr);
    encode_bytes_field(1, &sp_attr, &mut sp_req);
    let create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.CreateRequest", sp_req).await;
    assert_eq!(create_rsp.status_code, 200);
    let create_wm = WireMessage::decode(&create_rsp.payload.unwrap().value).unwrap();
    let id_wm = WireMessage::decode(create_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let plan_uuid: [u8; 16] = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, 200);

    // Track personal plan
    let (mut f2_id, mut f2_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut f2_id);
    encode_bytes_field(2, &f2_id, &mut f2_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.SetActiveRequest", f2_req).await.status_code, 200);
    let active_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetActiveRequest", Vec::new()).await;
    assert_eq!(active_rsp.status_code, 200);
    assert!(!active_rsp.payload.unwrap().value.is_empty(), "Active skill plan must be tracked");

    // Corporation skill plans via Quasar publicGatewaySvc
    let mut corp_attr = Vec::new();
    encode_string_field(1, "Corp Doctrine", &mut corp_attr);
    encode_string_field(2, "Fleet doctrine plan", &mut corp_attr);
    let mut corp_req_field = Vec::new();
    let mut corp_tid = Vec::new();
    encode_varint_field(1, 3300, &mut corp_tid);
    encode_bytes_field(1, &corp_tid, &mut corp_req_field);
    encode_varint_field(2, 4, &mut corp_req_field);
    encode_bytes_field(3, &corp_req_field, &mut corp_attr);
    let mut cat_id_b = Vec::new();
    encode_bytes_field(1, &[1u8; 16], &mut cat_id_b);
    encode_bytes_field(4, &cat_id_b, &mut corp_attr);

    let mut corp_create_val = Vec::new();
    encode_bytes_field(1, &corp_attr, &mut corp_create_val);
    let corp_create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.CreateRequest", corp_create_val).await;
    assert_eq!(corp_create_rsp.status_code, 200);

    let corp_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(corp_all_rsp.status_code, 200);

    // Certified doctrine skill plans loaded from SDE (res:/staticdata/skillplans.fsdbinary) alongside personal plans
    let all_plans_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(all_plans_rsp.status_code, 200);
    let all_wm = WireMessage::decode(&all_plans_rsp.payload.unwrap().value).unwrap();
    let cert_field = all_wm.fields.iter().find(|f| {
        let b = f.value.as_bytes().unwrap_or(&[]);
        WireMessage::decode(b).ok().and_then(|id_wm| {
            id_wm.fields.first().and_then(|id_f| id_f.value.as_bytes()).map(|u| u.starts_with(b"CERT"))
        }).unwrap_or(false)
    });
    assert!(cert_field.is_some(), "SDE certified skill plans must be returned in GetAllRequest");

    let id_wm = WireMessage::decode(cert_field.unwrap().value.as_bytes().unwrap()).unwrap();
    let cert_plan_uuid: [u8; 16] = id_wm.fields[0].value.as_bytes().unwrap().try_into().unwrap();
    let (mut cert_get_id, mut cert_get_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &cert_plan_uuid, &mut cert_get_id);
    encode_bytes_field(1, &cert_get_id, &mut cert_get_req);
    let cert_get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.GetRequest", cert_get_req).await;
    assert_eq!(cert_get_rsp.status_code, 200, "Certified skill plan must be queryable via GetRequest");

    // --- Accelerators (`feat:skills.accelerators`) ---
    // Compare training rate without accelerator vs with accelerator booster
    let booster_item_id = 991_000_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2; // Omega
        ch.neural = NeuralAttributes {
            charisma: 20, intelligence: 20, memory: 20, perception: 20, willpower: 20,
            bonus_remaps: 0, last_remap_unix: None, remap_cooldown_until_unix: None,
        };
        snap.skill_queues.insert(char_id, vec![
            eve_db::SkillQueueEntry {
                type_id: 3300, target_level: 2, start_sp: 0.0, end_sp: 10_000.0, duration_seconds: 20_000.0, started_at_unix: None,
            }
        ]);
    });

    // Advance 60 seconds with base attributes (20 + 20/2 = 30 SP/min -> 30 SP)
    ctx.advance_skill_queues(60.0);
    let base_sp = ctx.db.with_snapshot(|snap| snap.skill_queues[&char_id][0].start_sp);
    assert!((base_sp - 30.0).abs() < 1e-3, "Base attributes 20/20 should train at 30 SP/min, got {base_sp}");

    // Add cerebral accelerator booster: +10 all attributes (attributes 175..=179)
    ctx.db.with_mut_snapshot(|snap| {
        let mut booster_attrs = HashMap::new();
        for attr_id in 175..=179 {
            booster_attrs.insert(attr_id, 10.0);
        }
        snap.items.insert(booster_item_id, eve_db::InventoryItemRecord {
            item_id: booster_item_id,
            type_id: 45998,
            owner_id: char_id,
            location_id: char_id,
            flag: eve_db::InventoryFlag::Booster,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: booster_attrs,
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
    });

    // Advance another 60 seconds with accelerator: (30 + 30/2 = 45 SP/min -> +45 SP)
    ctx.advance_skill_queues(60.0);
    let accel_sp = ctx.db.with_snapshot(|snap| snap.skill_queues[&char_id][0].start_sp);
    let gained = accel_sp - base_sp;
    assert!((gained - 45.0).abs() < 1e-3, "With +10 accelerator booster, SP/min should be 45 SP/min, got {gained}");

    // Mark accelerator booster as expired (DYN_ATTR_BOOSTER_EXPIRY_UNIX = 1.0 < now)
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&booster_item_id).unwrap();
        it.dynamic_attributes.insert(eve_server::services::implant_plugging::DYN_ATTR_BOOSTER_EXPIRY_UNIX, 1.0);
    });
    // Advance another 60 seconds with expired accelerator: back to base 30 SP/min -> +30 SP
    ctx.advance_skill_queues(60.0);
    let expired_sp = ctx.db.with_snapshot(|snap| snap.skill_queues[&char_id][0].start_sp);
    let expired_gain = expired_sp - accel_sp;
    assert!((expired_gain - 30.0).abs() < 1e-3, "Expired booster must not accelerate training (expected 30 SP, got {expired_gain})");
}
