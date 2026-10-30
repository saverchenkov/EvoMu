//! Remediation integration tests for V24 SDE-backed Quasar Daily Goals & AIR Daily Operations
//! (`feat:pve.daily-goals`, `feat:quasar.dailygoal`,
//! `quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj`,
//! `quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, DynamicValue, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireField, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

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
    panic!("Expected KeyVal with response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("dg-remed-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"dg-remediation-01".to_vec(),
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

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn test_pve_and_quasar_daily_goals_sde_seeding_and_gameplay_progression() {
    // parity: feat:pve.daily-goals, feat:quasar.dailygoal, quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "feat:pve.daily-goals",
        "feat:quasar.dailygoal",
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001u32;
    let mut sess = SessionState::new(11_101);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id as i64);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Out-of-the-box ServerContext seeds all 5 V24 AIR Daily Operations from contributionmethods.fsdbinary
    let methods = ctx
        .sde
        .contribution_methods()
        .expect("SDE contribution_methods must load from contributionmethods.fsdbinary");
    let mut expected_titles = Vec::new();
    for key in [
        "KillNPC",
        "MineOre",
        "ScanSignature",
        "SpaceJump",
        "ManufactureItem",
    ] {
        let entry = methods.get(key).unwrap_or_else(|| {
            panic!(
                "SDE contribution_methods missing key {key}; available keys: {:?}",
                methods.keys().collect::<Vec<_>>()
            )
        });
        let title_id = entry
            .get("titleID")
            .and_then(|v| v.as_u64())
            .filter(|&v| v > 0)
            .unwrap_or_else(|| panic!("SDE contribution_methods[{key}] missing positive titleID")) as u32;
        expected_titles.push(title_id);
    }

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllCurrentResponse");
    let goal_fields: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(goal_fields.len(), 5, "ServerContext must seed 5 SDE daily goals without manual setup");

    let mut uuids = Vec::new();
    for f in &goal_fields {
        let id_dyn = reg
            .decode_dynamic("eve_public.dailygoal.Identifier", f.value.as_bytes().unwrap())
            .expect("decode Identifier");
        let Some(DynamicValue::Bytes(b)) = id_dyn.get(1) else {
            panic!("missing uuid bytes");
        };
        let mut arr = [0u8; 16];
        arr.copy_from_slice(b);
        uuids.push(arr);
    }

    // Verify GetRequest decodes real SDE contributionmethods.fsdbinary titleIDs & Paragon assigner 1_000_419
    let mut seen_title_ids = Vec::new();
    for uuid in &uuids {
        let get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetRequest", make_uuid_request(*uuid)).await;
        assert_eq!(get_rsp.status_code, STATUS_OK);
        let get_dyn = reg
            .decode_dynamic("eve_public.dailygoal.api.GetResponse", &get_rsp.payload.unwrap().value)
            .expect("decode GetResponse");
        let Some(DynamicValue::Message(attrs)) = get_dyn.get(1) else {
            panic!("missing goal Attributes");
        };
        let Some(DynamicValue::Message(name_msg)) = attrs.get(1) else {
            panic!("missing name_message");
        };
        let Some(DynamicValue::Uint32(title_id)) = name_msg.get(1) else {
            panic!("missing title_id");
        };
        seen_title_ids.push(*title_id);
        let Some(DynamicValue::Message(assigner_msg)) = attrs.get(4) else {
            panic!("missing assigner");
        };
        assert_eq!(assigner_msg.get(1), Some(&DynamicValue::Uint32(1_000_419)));
        assert!(matches!(get_dyn.get(4), Some(DynamicValue::Repeated(v)) if v.len() == 2));
    }
    for expected_title in expected_titles {
        assert!(
            seen_title_ids.contains(&expected_title),
            "Missing SDE contributionmethod titleID {expected_title} in {seen_title_ids:?}"
        );
    }

    // Subscribe to publicGatewaySvc for ProgressedNotice, CompletedNotice, and RedeemedNotice
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");
    let _ = drain_notices(&mut rx);

    let (initial_isk, initial_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(char_id as i64)];
        (ch.balance.0, ch.free_skill_points)
    });

    // 2. Trigger real gameplay pathways across all 5 daily goal categories:
    // (a) ScanSignature (target = 5)
    for sig_id in 800_001..=800_005 {
        ctx.mark_signature_scanned(char_id as i64, 30_000_142, sig_id);
    }
    let notices_scan = drain_notices(&mut rx);
    assert!(
        notices_scan
            .iter()
            .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.ProgressedNotice"),
        "ScanSignature must emit ProgressedNotice"
    );
    assert!(
        notices_scan
            .iter()
            .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.CompletedNotice"),
        "Completing ScanSignature target must emit CompletedNotice"
    );

    // (b) MineAsteroid (target = 2,000 units)
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(
        &ctx,
        char_id as i64,
        2_000,
        30_000_142,
        1230,
    );
    let _ = drain_notices(&mut rx);

    // (c) ManufactureItem (target = 1 job delivered via industryManager.CompleteJob)
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_001,
            InventoryItemRecord {
                item_id: 990_001,
                type_id: 691,
                owner_id: char_id as i64,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            77_001,
            IndustryJobRecord {
                job_id: 77_001,
                installer_id: char_id as i64,
                facility_id: 60_003_760,
                blueprint_item_id: 990_001,
                blueprint_type_id: 691,
                product_type_id: 587,
                activity_id: 1,
                runs: 1,
                duration_seconds: 60,
                start_unix: now.saturating_sub(120),
                status: "Active".to_string(),
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(77_001)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CompleteJob manufacturing");
    let _ = drain_notices(&mut rx);

    // (d) DestroyNPC (1 NPC kill -> partial progress 1/25)
    let npc_ball = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx,
        30_000_142,
        10035,
        "BeltRat",
        1,
        Default::default(),
    )
    .await;
    eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx,
        30_000_142,
        npc_ball,
        char_id as i64,
    )
    .await;
    let _ = drain_notices(&mut rx);

    // 3. Verify 3 completed goals (ScanSignature, MineAsteroid, ManufactureItem) and 1 incomplete goal (DestroyNPC)
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);
    let comp_wm = WireMessage::decode(&comp_rsp.payload.unwrap().value).expect("decode completed");
    assert_eq!(comp_wm.fields.iter().filter(|f| f.field_number == 1).count(), 3);

    let goals_now = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
    let combat_goal = goals_now.iter().find(|g| g.template.category == 1).unwrap();
    let scan_goal = goals_now.iter().find(|g| g.template.category == 3).unwrap();
    assert_eq!(combat_goal.current_progress, 1);
    assert!(!combat_goal.completed);
    assert!(scan_goal.completed && !scan_goal.claimed);

    // Negative check: redeeming incomplete DestroyNPC goal fails with 403 Forbidden
    let bad_redeem = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(combat_goal.uuid),
    )
    .await;
    assert_eq!(bad_redeem.status_code, STATUS_FORBIDDEN);

    // Positive RedeemRequest on ScanSignature -> credits ISK & SP and emits RedeemedNotice
    let redeem_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(scan_goal.uuid),
    )
    .await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    let notices_redeem = drain_notices(&mut rx);
    assert!(
        notices_redeem
            .iter()
            .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.RedeemedNotice"),
        "RedeemRequest must emit RedeemedNotice"
    );

    // Negative check: double-redeeming already-claimed goal fails with 403 Forbidden
    let dup_redeem = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(scan_goal.uuid),
    )
    .await;
    assert_eq!(dup_redeem.status_code, STATUS_FORBIDDEN);

    // RedeemAllRequest claims the remaining 2 completed goals (MineAsteroid + ManufactureItem)
    let redeem_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemAllRequest", Vec::new()).await;
    assert_eq!(redeem_all_rsp.status_code, STATUS_OK);

    let (after_isk, after_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(char_id as i64)];
        (ch.balance.0, ch.free_skill_points)
    });
    assert_eq!(after_isk, initial_isk + IskCents::from_isk(1_500_000).0);
    assert!((after_sp - (initial_sp + 15_000.0)).abs() < 1e-6);

    // 4. Advance virtual clock by 86,400s (next 11:00 UTC downtime bucket) -> daily goals reset cleanly
    ctx.advance_virtual_clock(86_400);
    let next_day_goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
    assert_eq!(next_day_goals.len(), 5);
    assert!(next_day_goals
        .iter()
        .all(|g| g.current_progress == 0 && !g.completed && !g.claimed));
}

#[tokio::test]
async fn test_daily_goals_reset_at_1100_utc_downtime_not_midnight() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    // Advance to 23:59:00 UTC on the next UTC day
    let next_utc_midnight = (ctx.now_unix() / 86_400 + 1) * 86_400;
    let t_2359_utc = next_utc_midnight + 86_340;
    ctx.advance_virtual_clock(t_2359_utc - ctx.now_unix());

    let char_id = 90_000_002u32;
    let mut sess = SessionState::new(11_102);
    sess.user_id = Some(2);
    sess.character_id = Some(char_id as i64);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    let goals_init = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals_at(char_id, ctx.now_unix());
    assert_eq!(goals_init.len(), 5);
    let scan_uuid = goals_init.iter().find(|g| g.template.category == 3).unwrap().uuid;

    // Progress & claim ScanSignature at 23:59 UTC
    ctx.quasar_gateway
        .progression
        .daily_goals
        .add_progress(char_id, &scan_uuid, 5, ctx.now_unix());
    let redeem_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(scan_uuid),
    )
    .await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);

    // Advance 120s to 00:01 UTC (2026-01-02 00:01:00 UTC): must NOT reset across 00:00 UTC
    ctx.advance_virtual_clock(120);
    let goals_0001 = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals_at(char_id, ctx.now_unix());
    let scan_0001 = goals_0001.iter().find(|g| g.uuid == scan_uuid).unwrap();
    assert!(scan_0001.claimed, "daily goal must NOT reset at 00:00 UTC");
    assert_eq!(scan_0001.current_progress, 5);

    // Advance to 10:59 UTC (10h 58m = 39_480s later): still same downtime bucket, must NOT reset
    ctx.advance_virtual_clock(39_480);
    let goals_1059 = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals_at(char_id, ctx.now_unix());
    let scan_1059 = goals_1059.iter().find(|g| g.uuid == scan_uuid).unwrap();
    assert!(scan_1059.claimed, "daily goal must NOT reset before 11:00 UTC downtime");

    // Advance 120s to 11:01 UTC: crosses 11:00 UTC downtime -> MUST reset
    ctx.advance_virtual_clock(120);
    let goals_1101 = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals_at(char_id, ctx.now_unix());
    let scan_1101 = goals_1101.iter().find(|g| g.uuid == scan_uuid).unwrap();
    assert!(!scan_1101.claimed, "daily goal MUST reset after 11:00 UTC downtime");
    assert!(!scan_1101.completed);
    assert_eq!(scan_1101.current_progress, 0);
}
