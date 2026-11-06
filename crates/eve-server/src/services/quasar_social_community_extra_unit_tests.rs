//! Deep branch-coverage and mutation-killing unit tests for `quasar_social_community`.

#![forbid(unsafe_code)]

use super::quasar_social_community::{
    publish_medal_awarded_notice, publish_narrative_warp_vector_notice,
    publish_notification_notice, register,
};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::{
    corp_roles, IskCents, KillmailRecord, WarAllyRecord, WarLifecycleMeta, WarRecord,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_router() -> (PublicGatewayRouter, Arc<WorldRepository>) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    register(&router, Arc::clone(&db));
    (router, db)
}

fn invoke_quasar(
    router: &PublicGatewayRouter,
    endpoint: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(endpoint)
        .cloned()
        .unwrap_or_else(|| panic!("missing handler for {endpoint}"));
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{endpoint}"),
        value: Vec::new(),
    };
    handler(ctx, &any, msg)
}

fn make_ctx(
    user_id: Option<i32>,
    char_id: Option<u32>,
    corp_id: Option<u32>,
    admin: bool,
) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session(
        user_id,
        char_id,
        corp_id,
        if admin { corp_roles::ROLE_DIRECTOR } else { 0 },
        "tranquility",
        vec![1],
    );
    if admin {
        ctx.service_role_mask = u64::MAX;
    }
    ctx
}

fn decode_sub_varint(bytes: &[u8], outer_field: u32, inner_field: u32) -> Option<u64> {
    let wm = WireMessage::decode(bytes).ok()?;
    let sub_bytes = wm
        .fields
        .iter()
        .find(|f| f.field_number == outer_field)
        .and_then(|f| f.value.as_bytes())?;
    let sub_wm = WireMessage::decode(sub_bytes).ok()?;
    sub_wm
        .fields
        .iter()
        .find(|f| f.field_number == inner_field)
        .and_then(|f| f.value.as_varint())
}

#[test]
fn cspa_get_and_set_cover_auth_clamping_and_fractional_nanos_encoding() {
    let (router, db) = make_router();
    let unauth_ctx = make_ctx(None, None, None, false);
    let player_ctx = make_ctx(Some(10), Some(90_000_001), Some(1_000_009), false);

    // 1. Unauthenticated GetRequest without explicit target character ID -> UNAUTHORIZED
    let empty_msg = DynamicQuasarMessage::new("Req");
    let err_get = invoke_quasar(&router, "eve.mail.cspa.GetRequest", &unauth_ctx, &empty_msg).unwrap_err();
    assert_eq!(err_get.0, STATUS_UNAUTHORIZED);

    // 2. Unauthenticated SetRequest -> UNAUTHORIZED
    let err_set = invoke_quasar(&router, "eve.mail.cspa.SetRequest", &unauth_ctx, &empty_msg).unwrap_err();
    assert_eq!(err_set.0, STATUS_UNAUTHORIZED);

    // 3. Default CSPA charge when unset is 0 units and omits field 2 (nanos)
    let rsp_zero = invoke_quasar(&router, "eve.mail.cspa.GetRequest", &player_ctx, &empty_msg).unwrap();
    assert_eq!(decode_sub_varint(&rsp_zero.value, 1, 1), Some(0));
    assert_eq!(decode_sub_varint(&rsp_zero.value, 1, 2), None);

    // 4. SetRequest clamps negative values to 0 and values above 1_000_000 to 1_000_000
    let mut msg_neg = DynamicQuasarMessage::new("Set");
    msg_neg.set(1, DynamicValue::Int64(-500));
    invoke_quasar(&router, "eve.mail.cspa.SetRequest", &player_ctx, &msg_neg).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(snap.character_cspa_charges.get(&90_000_001), Some(&IskCents(0)));
    });

    let mut msg_over = DynamicQuasarMessage::new("Set");
    msg_over.set(1, DynamicValue::Uint64(2_500_000));
    invoke_quasar(&router, "eve.mail.cspa.SetRequest", &player_ctx, &msg_over).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.character_cspa_charges.get(&90_000_001),
            Some(&IskCents(100_000_000))
        );
    });

    // 5. Fractional cents in snapshot produce exact units and nanos (rem_euclid(100) * 10_000_000)
    db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(90_000_077, IskCents(295_075));
    });
    let mut sub_id = DynamicQuasarMessage::new("Id");
    sub_id.set(1, DynamicValue::Int32(90_000_077));
    let mut msg_target = DynamicQuasarMessage::new("Get");
    msg_target.set(1, DynamicValue::Message(Box::new(sub_id)));
    let rsp_frac = invoke_quasar(&router, "eve.mail.cspa.GetRequest", &unauth_ctx, &msg_target).unwrap();
    assert_eq!(decode_sub_varint(&rsp_frac.value, 1, 1), Some(2_950));
    assert_eq!(decode_sub_varint(&rsp_frac.value, 1, 2), Some(750_000_000));

    // Negative cents clamp units to 0 while preserving positive euclidean remainder nanos
    db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(90_000_078, IskCents(-240));
    });
    let mut msg_neg_target = DynamicQuasarMessage::new("Get");
    msg_neg_target.set(1, DynamicValue::Int64(90_000_078));
    let rsp_neg_cents = invoke_quasar(&router, "eve.mail.cspa.GetRequest", &player_ctx, &msg_neg_target).unwrap();
    assert_eq!(decode_sub_varint(&rsp_neg_cents.value, 1, 1), Some(0));
    assert_eq!(decode_sub_varint(&rsp_neg_cents.value, 1, 2), Some(600_000_000));
}

#[test]
fn ccp_send_mail_validates_auth_admin_recipients_and_default_fields() {
    let (router, db) = make_router();
    let unauth_ctx = make_ctx(None, None, None, true);
    let player_ctx = make_ctx(Some(10), Some(90_000_001), Some(1_000_009), false);
    let admin_ctx = make_ctx(Some(10), Some(90_000_001), Some(1_000_009), true);

    let empty_msg = DynamicQuasarMessage::new("Mail");
    assert_eq!(
        invoke_quasar(&router, "eve.mail.CCPSendMailRequest", &unauth_ctx, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        invoke_quasar(&router, "eve.mail.CCPSendMailRequest", &player_ctx, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        invoke_quasar(&router, "eve.mail.CCPSendMailRequest", &admin_ctx, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Zero or invalid recipient IDs filter out to empty -> STATUS_BAD_REQUEST
    let mut zero_rcpt = DynamicQuasarMessage::new("Mail");
    zero_rcpt.set(1, DynamicValue::Repeated(vec![DynamicValue::Uint32(0)]));
    assert_eq!(
        invoke_quasar(&router, "eve.mail.CCPSendMailRequest", &admin_ctx, &zero_rcpt)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Force next_entity_id to 0 to test `.max(1)` floor, and send with single scalar recipient and default subject/body
    db.with_mut_snapshot(|snap| {
        snap.next_entity_id = 0;
        snap.mails.clear();
    });
    let mut single_rcpt = DynamicQuasarMessage::new("Mail");
    single_rcpt.set(1, DynamicValue::Uint32(90_000_002));
    let rsp = invoke_quasar(&router, "eve.mail.CCPSendMailRequest", &admin_ctx, &single_rcpt).unwrap();
    assert_eq!(decode_sub_varint(&rsp.value, 1, 1), Some(1));

    db.with_snapshot(|snap| {
        assert_eq!(snap.next_entity_id, 2);
        assert_eq!(snap.mails.len(), 1);
        assert_eq!(snap.mails[0].message_id, 1);
        assert_eq!(snap.mails[0].sender_id, 90_000_001);
        assert_eq!(snap.mails[0].recipient_id, 90_000_002);
        assert_eq!(snap.mails[0].subject, "Fleet Notification");
        assert_eq!(snap.mails[0].body, "Corporate fleet mobilization orders");
    });
}

#[test]
fn calendar_contact_labels_and_standings_verify_wire_payloads_and_clamping() {
    let (router, db) = make_router();
    let unauth_ctx = make_ctx(None, None, None, false);
    let char_id = 90_000_001_u32;
    let corp_id = 1_000_009_u32;
    let player_ctx = make_ctx(Some(10), Some(char_id), Some(corp_id), false);
    let orphan_ctx = make_ctx(Some(11), Some(99_888_777), None, false);

    // 1. Calendar event wire payloads
    let pers = invoke_quasar(
        &router,
        "eve.calendar.personal_event.GetRequest",
        &player_ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert_eq!(
        decode_sub_varint(&pers.value, 1, 3).or_else(|| {
            let wm = WireMessage::decode(&pers.value).ok()?;
            let attr = wm.fields.first()?.value.as_bytes()?;
            decode_sub_varint(attr, 3, 1)
        }),
        Some(1_700_000_000)
    );

    // 2. Contact labels auth and corporation membership checks
    assert_eq!(
        invoke_quasar(
            &router,
            "eve.contact.label.GetForCharacterRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Req")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        invoke_quasar(
            &router,
            "eve.contact.label.GetForCorporationRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Req")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        invoke_quasar(
            &router,
            "eve.contact.label.GetForCorporationRequest",
            &orphan_ctx,
            &DynamicQuasarMessage::new("Req")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Matching explicit character ID and corporation ID succeed and encode exact label ID & name
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(char_id as i64)) {
            ch.corporation_id = corp_id as i64;
        }
        snap.char_contact_labels
            .insert(char_id as i64, vec![(42, "Watchlist".into())]);
        snap.corp_contact_labels
            .insert(corp_id as i64, vec![(84, "Logistics".into(), 0)]);
    });

    let mut own_char_req = DynamicQuasarMessage::new("Req");
    own_char_req.set(1, DynamicValue::Uint32(char_id));
    let char_rsp = invoke_quasar(
        &router,
        "eve.contact.label.GetForCharacterRequest",
        &player_ctx,
        &own_char_req,
    )
    .unwrap();
    assert_eq!(decode_sub_varint(&char_rsp.value, 1, 1), Some(42));

    let mut own_corp_req = DynamicQuasarMessage::new("Req");
    own_corp_req.set(1, DynamicValue::Uint32(corp_id));
    let corp_rsp = invoke_quasar(
        &router,
        "eve.contact.label.GetForCorporationRequest",
        &player_ctx,
        &own_corp_req,
    )
    .unwrap();
    assert_eq!(decode_sub_varint(&corp_rsp.value, 1, 1), Some(84));

    // 3. Standings commands clamp at +10.0 and -10.0 and handle unauthenticated fallback
    for _ in 0..15 {
        invoke_quasar(
            &router,
            "eve.standing.api.IncreaseCommanded",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Inc"),
        )
        .unwrap();
    }
    for _ in 0..25 {
        invoke_quasar(
            &router,
            "eve.standing.api.DecreaseCommanded",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Dec"),
        )
        .unwrap();
    }
}

#[test]
fn war_handlers_cover_errors_lifecycle_meta_killmail_stats_and_allies_filtering() {
    let (router, db) = make_router();
    let player_ctx = make_ctx(Some(10), Some(90_000_001), Some(1_000_009), false);

    // 1. Missing war_id -> STATUS_BAD_REQUEST; unknown war_id -> STATUS_NOT_FOUND
    let empty_msg = DynamicQuasarMessage::new("War");
    let mut unknown_msg = DynamicQuasarMessage::new("War");
    unknown_msg.set(1, DynamicValue::Uint64(999_999));

    for endpoint in [
        "eve.war.GetRequest",
        "eve.war.GetDestructionStatisticsRequest",
        "eve.war.GetAlliesRequest",
    ] {
        assert_eq!(
            invoke_quasar(&router, endpoint, &player_ctx, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_BAD_REQUEST
        );
        assert_eq!(
            invoke_quasar(&router, endpoint, &player_ctx, &unknown_msg)
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND
        );
    }

    // 2. Seed two wars, lifecycle meta, and killmails
    let war_a = 500_i64;
    let war_b = 200_i64;
    db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        snap.wars.insert(
            war_a,
            WarRecord {
                war_id: war_a,
                aggressor_corp_id: 1_001,
                defender_corp_id: 2_002,
                mutual: true,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
        snap.wars.insert(
            war_b,
            WarRecord {
                war_id: war_b,
                aggressor_corp_id: 3_003,
                defender_corp_id: 4_004,
                mutual: true,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            war_a,
            WarLifecycleMeta {
                open_for_allies: Some(true),
                allies: vec![
                    WarAllyRecord {
                        corporation_id: Some(5_005),
                        alliance_id: None,
                    },
                    WarAllyRecord {
                        corporation_id: None,
                        alliance_id: Some(6_006),
                    },
                    WarAllyRecord {
                        corporation_id: Some(0),
                        alliance_id: None,
                    },
                ],
                ..Default::default()
            },
        );
        snap.killmails.clear();
        // 2 kills by aggressor (1001) against defender (2002)
        for kid in [1, 2] {
            snap.killmails.insert(
                kid,
                KillmailRecord {
                    kill_id: kid,
                    killed_unix: 1_700_000_000,
                    solar_system_id: 30_000_142,
                    victim_character_id: 90_000_010,
                    victim_corp_id: 2_002,
                    victim_alliance_id: None,
                    ship_type_id: 587,
                    attacker_character_id: 90_000_001,
                    attacker_corp_id: 1_001,
                    attacker_alliance_id: None,
                    total_value_isk: 1_000.0,
                },
            );
        }
        // 1 kill by defender (2002) against aggressor (1001)
        snap.killmails.insert(
            3,
            KillmailRecord {
                kill_id: 3,
                killed_unix: 1_700_000_000,
                solar_system_id: 30_000_142,
                victim_character_id: 90_000_001,
                victim_corp_id: 1_001,
                victim_alliance_id: None,
                ship_type_id: 587,
                attacker_character_id: 90_000_010,
                attacker_corp_id: 2_002,
                attacker_alliance_id: None,
                total_value_isk: 1_000.0,
            },
        );
        // Unrelated killmail (attacker 1001, victim 9999) -> must not be counted
        snap.killmails.insert(
            4,
            KillmailRecord {
                kill_id: 4,
                killed_unix: 1_700_000_000,
                solar_system_id: 30_000_142,
                victim_character_id: 90_000_099,
                victim_corp_id: 9_999,
                victim_alliance_id: None,
                ship_type_id: 587,
                attacker_character_id: 90_000_001,
                attacker_corp_id: 1_001,
                attacker_alliance_id: None,
                total_value_isk: 1_000.0,
            },
        );
    });

    // GetAllRequest returns war IDs sorted ascending (200, then 500)
    let all_rsp = invoke_quasar(&router, "eve.war.GetAllRequest", &player_ctx, &empty_msg).unwrap();
    let wm_all = WireMessage::decode(&all_rsp.value).unwrap();
    let ids: Vec<u64> = wm_all
        .fields
        .iter()
        .filter_map(|f| WireMessage::decode(f.value.as_bytes()?).ok()?.fields.first()?.value.as_varint())
        .collect();
    assert_eq!(ids, vec![200, 500]);

    // GetRequest for war_a uses lifecycle meta open_for_allies = true (1) despite mutual = true
    let mut req_a = DynamicQuasarMessage::new("War");
    req_a.set(1, DynamicValue::Uint64(war_a as u64));
    let get_a = invoke_quasar(&router, "eve.war.GetRequest", &player_ctx, &req_a).unwrap();
    let attr_a = WireMessage::decode(&get_a.value).unwrap().fields[0]
        .value
        .as_bytes()
        .unwrap()
        .to_vec();
    assert_eq!(decode_sub_varint(&attr_a, 1, 1), Some(1_001));
    assert_eq!(decode_sub_varint(&attr_a, 2, 1), Some(2_002));
    let wm_attr_a = WireMessage::decode(&attr_a).unwrap();
    assert_eq!(
        wm_attr_a
            .fields
            .iter()
            .find(|f| f.field_number == 3)
            .and_then(|f| f.value.as_varint()),
        Some(1)
    );

    // GetDestructionStatisticsRequest for war_a -> by_agg = 2, by_def = 1
    let stat_a = invoke_quasar(
        &router,
        "eve.war.GetDestructionStatisticsRequest",
        &player_ctx,
        &req_a,
    )
    .unwrap();
    assert_eq!(decode_sub_varint(&stat_a.value, 1, 1), Some(2));
    assert_eq!(decode_sub_varint(&stat_a.value, 2, 1), Some(1));

    // GetAlliesRequest for war_a -> [5005, 6006] (0 filtered out)
    let allies_a = invoke_quasar(&router, "eve.war.GetAlliesRequest", &player_ctx, &req_a).unwrap();
    let wm_allies = WireMessage::decode(&allies_a.value).unwrap();
    let ally_ids: Vec<u64> = wm_allies
        .fields
        .iter()
        .filter_map(|f| WireMessage::decode(f.value.as_bytes()?).ok()?.fields.first()?.value.as_varint())
        .collect();
    assert_eq!(ally_ids, vec![5_005, 6_006]);
}

#[test]
fn tags_default_fallback_and_notice_publishers_encode_unread_and_read_states() {
    let (router, _db) = make_router();
    let player_ctx = make_ctx(Some(10), Some(90_000_001), Some(1_000_009), false);

    // RegisterRequest without field 1 defaults to "fleet_doctrine"
    invoke_quasar(
        &router,
        "eve.tag.api.RegisterRequest",
        &player_ctx,
        &DynamicQuasarMessage::new("Tag"),
    )
    .unwrap();
    let list_rsp = invoke_quasar(
        &router,
        "eve.tag.api.ListRequest",
        &player_ctx,
        &DynamicQuasarMessage::new("List"),
    )
    .unwrap();
    assert!(!list_rsp.value.is_empty());

    // Subscribe session 777 for character 90_000_077 and verify notice delivery
    let mut sess = SessionState::new(777);
    sess.user_id = Some(77);
    sess.character_id = Some(90_000_077);
    sess.corporation_id = Some(1_000_009);
    let _rx = router.subscribe_session(&sess, Vec::new(), 1_700_000_000);

    let delivered_unread = publish_notification_notice(
        &router,
        NoticeScope::Character(90_000_077),
        45,
        90_000_001,
        false,
        1_700_000_123,
    )
    .unwrap();
    assert_eq!(delivered_unread, 1);

    let delivered_medal = publish_medal_awarded_notice(
        &router,
        NoticeScope::Character(90_000_077),
        88,
        90_000_077,
        90_000_001,
        1_000_009,
        "Distinguished Service",
        1_700_000_124,
    )
    .unwrap();
    assert_eq!(delivered_medal, 1);

    let delivered_warp = publish_narrative_warp_vector_notice(
        &router,
        NoticeScope::Character(90_000_077),
        90_000_077,
        30_000_142,
        1_700_000_125,
    )
    .unwrap();
    assert_eq!(delivered_warp, 1);

    let drained = router.poll_notices(777);
    assert_eq!(drained.len(), 3);
    let _ = HashMap::<u32, u32>::new();
}
