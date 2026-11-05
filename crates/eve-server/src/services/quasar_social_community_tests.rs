//! Unit tests for `quasar_social_community` (Mail, Calendar, Contact Labels, Standings, Wars, Tags, Notices).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::{corp_roles, WarRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn community_helpers_and_notice_publishers() {
    assert_eq!(extract_id(Some(&DynamicValue::Uint64(123))), Some(123));
    assert_eq!(extract_ids(Some(&DynamicValue::Uint32(456))), vec![456]);
    assert_eq!(
        extract_ids(Some(&DynamicValue::Repeated(vec![
            DynamicValue::Uint32(10),
            DynamicValue::Uint32(20),
        ]))),
        vec![10, 20]
    );

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));

    assert!(publish_medal_awarded_notice(
        &router,
        NoticeScope::Broadcast,
        1,
        90_000_001,
        90_000_002,
        1_000_009,
        "Valor",
        1_700_000_000,
    )
    .is_ok());

    assert!(publish_narrative_warp_vector_notice(
        &router,
        NoticeScope::Broadcast,
        90_000_001,
        30_000_142,
        1_700_000_000,
    )
    .is_ok());

    assert!(publish_notification_notice(
        &router,
        NoticeScope::Broadcast,
        10,
        90_000_001,
        true,
        1_700_000_000,
    )
    .is_ok());
}

#[test]
fn mail_calendar_contacts_standings_wars_and_tags_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 90_000_001_u32;
    let corp_id = 1_000_009_u32;
    let war_id = 777_001_i64;
    db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: corp_id as i64,
                defender_corp_id: 1_000_010,
                mutual: false,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
        snap.char_contact_labels
            .entry(cid as i64)
            .or_default()
            .push((1, "Friends".into()));
        snap.corp_contact_labels
            .entry(corp_id as i64)
            .or_default()
            .push((2, "Allies".into(), 0));
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    let player_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(cid),
        Some(corp_id),
        0,
        "tranquility",
        vec![1],
    );

    // Mail CSPA Set & Get
    let mut set_cspa = DynamicQuasarMessage::new("SetCspa");
    set_cspa.fields.insert(1, DynamicValue::Uint64(2_950));
    handlers["eve.mail.cspa.SetRequest"](&player_ctx, &dummy_any, &set_cspa).expect("set cspa");
    let get_cspa = handlers["eve.mail.cspa.GetRequest"](&player_ctx, &dummy_any, &DynamicQuasarMessage::new("Get"))
        .expect("get cspa");
    assert!(get_cspa.type_url.ends_with("eve.mail.cspa.GetResponse"));
    assert!(!get_cspa.value.is_empty());

    // CCPSendMailRequest: non-admin denied, admin succeeds
    let mut mail_msg = DynamicQuasarMessage::new("Mail");
    mail_msg
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Uint32(90_000_002)]));
    mail_msg.fields.insert(3, DynamicValue::String("Alert".into()));
    mail_msg.fields.insert(4, DynamicValue::String("CTA now".into()));
    assert_eq!(
        handlers["eve.mail.CCPSendMailRequest"](&player_ctx, &dummy_any, &mail_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let sent = handlers["eve.mail.CCPSendMailRequest"](&gm_ctx, &dummy_any, &mail_msg)
        .expect("admin send mail");
    assert!(sent.type_url.ends_with("eve.mail.CCPSendMailResponse"));

    // Calendar events
    for (req, suffix) in [
        ("eve.calendar.personal_event.GetRequest", "eve.calendar.personal_event.GetResponse"),
        ("eve.calendar.corporation_event.GetRequest", "eve.calendar.corporation_event.GetResponse"),
        ("eve.calendar.alliance_event.GetRequest", "eve.calendar.alliance_event.GetResponse"),
    ] {
        let r = handlers[req](&player_ctx, &dummy_any, &DynamicQuasarMessage::new("Req")).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Contact labels (own vs cross-entity denied)
    let char_lbl = handlers["eve.contact.label.GetForCharacterRequest"](
        &player_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("own char labels");
    assert!(char_lbl
        .type_url
        .ends_with("eve.contact.label.GetForCharacterResponse"));

    let mut other_msg = DynamicQuasarMessage::new("Req");
    other_msg.fields.insert(1, DynamicValue::Uint64(999_999));
    assert_eq!(
        handlers["eve.contact.label.GetForCharacterRequest"](&player_ctx, &dummy_any, &other_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let corp_lbl = handlers["eve.contact.label.GetForCorporationRequest"](
        &player_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("own corp labels");
    assert!(corp_lbl
        .type_url
        .ends_with("eve.contact.label.GetForCorporationResponse"));
    assert_eq!(
        handlers["eve.contact.label.GetForCorporationRequest"](&player_ctx, &dummy_any, &other_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Standings commands
    for req in [
        "eve.standing.api.IncreaseCommanded",
        "eve.standing.api.commands.IncreaseCommanded",
        "eve.standing.api.DecreaseCommanded",
        "eve.standing.api.commands.DecreaseCommanded",
    ] {
        handlers[req](&player_ctx, &dummy_any, &DynamicQuasarMessage::new("Req")).expect(req);
    }

    // Wars
    let mut war_msg = DynamicQuasarMessage::new("WarReq");
    war_msg.fields.insert(1, DynamicValue::Uint64(war_id as u64));
    for (req, suffix) in [
        ("eve.war.GetRequest", "eve.war.GetResponse"),
        ("eve.war.GetAllRequest", "eve.war.GetAllResponse"),
        (
            "eve.war.GetDestructionStatisticsRequest",
            "eve.war.GetDestructionStatisticsResponse",
        ),
        ("eve.war.GetAlliesRequest", "eve.war.GetAlliesResponse"),
        ("eve.war.GetKillMailsRequest", "eve.war.GetKillMailsResponse"),
    ] {
        let r = handlers[req](&player_ctx, &dummy_any, &war_msg).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Tags and tag versions
    let mut tag_reg = DynamicQuasarMessage::new("TagReg");
    tag_reg.fields.insert(1, DynamicValue::String("armor_brawl".into()));
    for (req, suffix) in [
        ("eve.tag.api.RegisterRequest", "eve.tag.api.RegisterResponse"),
        ("eve.tag.api.EditRequest", "eve.tag.api.EditResponse"),
        ("eve.tag.api.ValidateRequest", "eve.tag.api.ValidateResponse"),
        ("eve.tag.api.ListRequest", "eve.tag.api.ListResponse"),
        ("eve.tag.version.api.ListRequest", "eve.tag.version.api.ListResponse"),
        ("eve.tag.version.api.CreateRequest", "eve.tag.version.api.CreateResponse"),
        ("eve.tag.version.api.PromoteRequest", "eve.tag.version.api.PromoteResponse"),
    ] {
        let r = handlers[req](&player_ctx, &dummy_any, &tag_reg).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }
}
