use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;

fn make_auth_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30000142);
    s.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    s.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    s
}

fn make_public_req(type_name: &str, value: Vec<u8>, now_unix: u64) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-social-0001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-instance-001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn encode_seq_id(field_num: u32, id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_varint_field(1, id, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, &mut out);
    out
}

#[tokio::test]
async fn test_quasar_corporation_goals_complete() {
    // parity: feat:quasar.corporationgoal, quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.corporationgoal",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(105, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.entry(98000001).or_insert_with(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = 98000001;
            c
        });
        corp.ceo_id = 90000001;
        corp.master_wallet = eve_db::IskCents::from_isk(10_000_000);
    });

    // 1. Non-director cannot create a corporation goal; CEO creates goal with 500,000 ISK reward_pools debiting master_wallet
    let mut unauth_sess = make_auth_session(199, 2, 90000002, 98000001);
    unauth_sess.role = 0;
    unauth_sess.corp_role = 0;
    let mut create_goal = Vec::new();
    encode_string_field(1, "Mine Veldspar", &mut create_goal);
    encode_string_field(2, "Deliver ore to corp hangar", &mut create_goal);
    encode_varint_field(3, 10, &mut create_goal);
    let (mut curr_b, mut isk_b, mut pool_b) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, eve_proto::quasar::encode_zigzag64(500_000), &mut curr_b);
    encode_bytes_field(1, &curr_b, &mut isk_b);
    encode_bytes_field(2, &isk_b, &mut pool_b);
    encode_bytes_field(8, &pool_b, &mut create_goal);

    let unauth_cg = ctx.quasar_gateway.handle_public_request(
        &unauth_sess, &make_public_req("eve_public.goal.api.CreateRequest", create_goal.clone(), now), None, false, now,
    );
    assert_eq!(unauth_cg.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

    let cg_rsp = ctx.quasar_gateway.handle_public_request(
        &sess, &make_public_req("eve_public.goal.api.CreateRequest", create_goal, now), None, false, now,
    );
    assert_eq!(cg_rsp.status_code, STATUS_OK);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&98000001].master_wallet),
        eve_db::IskCents::from_isk(9_500_000),
        "CreateRequest with reward_pools must debit corp master_wallet"
    );
    let cg_wm = eve_proto::quasar::WireMessage::decode(&cg_rsp.payload.unwrap().value).unwrap();
    let goal_id_bytes = cg_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();
    let mut goal_req_bytes = Vec::new();
    encode_bytes_field(1, &goal_id_bytes, &mut goal_req_bytes);

    // Server-observed mining gameplay advances the active corporation goal by 5 units
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx, 90000001, 100_000_000_101, 30_000_142, eve_db::InventoryFlag::Cargo, 1230, 5,
    );

    // 2. GetAllRequest, GetActiveRequest, GetMyContributorSummaryForGoalRequest, GetMineWithRewardsRequest
    let all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.GetAllRequest", Vec::new(), now), None, false, now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(!all_rsp.payload.unwrap().value.is_empty());

    let act_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.GetActiveRequest", Vec::new(), now), None, false, now);
    assert_eq!(act_rsp.status_code, STATUS_OK);
    assert!(!act_rsp.payload.unwrap().value.is_empty());

    let summ_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest", goal_req_bytes.clone(), now), None, false, now);
    assert_eq!(summ_rsp.status_code, STATUS_OK);
    let summ_wm = eve_proto::quasar::WireMessage::decode(&summ_rsp.payload.unwrap().value).unwrap();
    let summary_inner = eve_proto::quasar::WireMessage::decode(summ_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(summary_inner.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(5));

    let rew_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Vec::new(), now), None, false, now);
    assert_eq!(rew_rsp.status_code, STATUS_OK);
    assert!(!rew_rsp.payload.unwrap().value.is_empty());

    // 3. RedeemMyRewardsRequest -> exact 50,000 ISK payout + CorporationGoalReward journal + RedeemedNotice
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&90000001].balance.0);
    let red_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.RedeemMyRewardsRequest", goal_req_bytes.clone(), now), None, false, now);
    assert_eq!(red_rsp.status_code, STATUS_OK, "Redeem failed: {}", red_rsp.status_message);
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&90000001].balance.0);
    assert_eq!(isk_after - isk_before, eve_db::IskCents::from_isk(50_000).0);
    let has_goal_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|e| e.character_id == 90000001 && e.entry_type == "CorporationGoalReward" && e.amount == eve_db::IskCents::from_isk(50_000))
    });
    assert!(has_goal_journal, "RedeemMyRewardsRequest must append CorporationGoalReward journal entry");
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("RedeemedNotice")).unwrap_or(false)));

    // Close first goal (refunding remaining 450,000 ISK escrow), then create second goal with 100,000 ISK escrow, mine 15 units (clamped to target=10), and redeem via RedeemAllMyRewardsRequest
    ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.goal.api.CloseRequest", goal_req_bytes, now), None, false, now);
    let (mut create_goal2, mut curr2, mut isk2, mut pool2) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Mine More Veldspar", &mut create_goal2);
    encode_varint_field(3, 10, &mut create_goal2);
    encode_varint_field(1, eve_proto::quasar::encode_zigzag64(100_000), &mut curr2);
    encode_bytes_field(1, &curr2, &mut isk2);
    encode_bytes_field(2, &isk2, &mut pool2);
    encode_bytes_field(8, &pool2, &mut create_goal2);
    let cg2_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.goal.api.CreateRequest", create_goal2, now), None, false, now);
    assert_eq!(cg2_rsp.status_code, STATUS_OK);
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx, 90000001, 100_000_000_101, 30_000_142, eve_db::InventoryFlag::Cargo, 1230, 15,
    );

    let red_all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.corporationgoal.api.RedeemAllMyRewardsRequest", Vec::new(), now), None, false, now);
    assert_eq!(red_all_rsp.status_code, STATUS_OK, "Redeem all failed: {}", red_all_rsp.status_message);
    let isk_after_all = ctx.db.with_snapshot(|s| s.characters[&90000001].balance.0);
    assert_eq!(isk_after_all - isk_after, eve_db::IskCents::from_isk(100_000).0);
}

#[tokio::test]
async fn test_quasar_freelance_complete() {
    // parity: feat:quasar.freelance, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.freelance",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(106, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.entry(98000001).or_insert_with(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = 98000001;
            c.ceo_id = 90000001;
            c
        });
        corp.ceo_id = 90000001;
        corp.master_wallet = eve_db::IskCents::from_f64_isk(100_000_000.0);
        if let Some(ch) = s.characters.get_mut(&90000001) {
            ch.station_id = Some(60003760);
        }

        s.items.remove(&100_000_009);
        s.items.insert(
            777888999,
            eve_db::InventoryItemRecord {
                item_id: 777888999,
                type_id: 35,
                owner_id: 90000001,
                location_id: 60003760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                ..Default::default()
            },
        );
    });

    // 1. Create project -> missing configuration rejected with 400; valid configuration (Pyerite type_id=35, batch_size=10) succeeds
    let mut create_bytes = Vec::new();
    encode_string_field(1, "Strategic Mineral Supply", &mut create_bytes);
    encode_string_field(2, "Deliver minerals for capital production", &mut create_bytes);
    encode_varint_field(3, 50, &mut create_bytes); // target_progress = 50
    let bad_create_req = make_public_req("eve_public.freelance.project.api.CreateRequest", create_bytes.clone(), now);
    let bad_create_rsp = ctx.quasar_gateway.handle_public_request(&sess, &bad_create_req, None, false, now);
    assert_eq!(bad_create_rsp.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);

    let (mut cfg_b, mut param_b, mut opts_b, mut mod_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "35", &mut opts_b);
    encode_bytes_field(3, &opts_b, &mut param_b);
    encode_bytes_field(3, &param_b, &mut cfg_b);
    encode_bytes_field(6, &cfg_b, &mut create_bytes);
    encode_varint_field(4, 10, &mut mod_b);
    encode_bytes_field(7, &mod_b, &mut create_bytes);
    let create_req = make_public_req("eve_public.freelance.project.api.CreateRequest", create_bytes, now);
    let create_rsp = ctx.quasar_gateway.handle_public_request(&sess, &create_req, None, false, now);
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let create_payload = create_rsp.payload.expect("CreateResponse payload");
    assert!(create_payload.type_url.contains("CreateResponse"));
    let created_wm = eve_proto::quasar::WireMessage::decode(&create_payload.value).expect("decode CreateResponse");
    let commit_bytes = created_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();

    // 2. Unknown project returns STATUS_NOT_FOUND; CommitRequest on the newly created project -> ParticipantAddedNotice
    let (mut unknown_req_b, mut unknown_id_b) = (Vec::new(), Vec::new());
    encode_bytes_field(1, b"unknown-proj-999", &mut unknown_id_b);
    encode_bytes_field(1, &unknown_id_b, &mut unknown_req_b);
    let unknown_rsp = ctx.quasar_gateway.handle_public_request(
        &sess, &make_public_req("eve_public.freelance.project.api.GetRequest", unknown_req_b, now), None, false, now,
    );
    assert_eq!(unknown_rsp.status_code, eve_server::quasar_gateway::STATUS_NOT_FOUND);

    let commit_req = make_public_req("eve_public.freelance.project.api.CommitRequest", commit_bytes.clone(), now);
    let commit_rsp = ctx.quasar_gateway.handle_public_request(&sess, &commit_req, None, false, now);
    assert_eq!(commit_rsp.status_code, STATUS_OK);
    assert!(commit_rsp.payload.unwrap().type_url.contains("CommitResponse"));

    let part_req = make_public_req("eve_public.freelance.project.api.GetParticipationDetailsRequest", commit_bytes.clone(), now);
    let part_rsp = ctx.quasar_gateway.handle_public_request(&sess, &part_req, None, false, now);
    assert_eq!(part_rsp.status_code, STATUS_OK);
    let part_wm = eve_proto::quasar::WireMessage::decode(&part_rsp.payload.unwrap().value).unwrap();
    let part_details = eve_proto::quasar::WireMessage::decode(part_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(part_details.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1));

    // 3. DeliverRequest (30 Pyerite = 3 batches of 10) on the created project
    let mut deliver_bytes = commit_bytes.clone();
    encode_varint_field(4, 30, &mut deliver_bytes);
    let deliver_req = make_public_req("eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver_bytes, now);
    let deliver_rsp = ctx.quasar_gateway.handle_public_request(&sess, &deliver_req, None, false, now);
    assert_eq!(deliver_rsp.status_code, STATUS_OK);
    assert!(deliver_rsp.payload.unwrap().type_url.contains("DeliverResponse"));

    let qty_after = ctx.db.with_snapshot(|s| s.items[&777888999].quantity);
    assert_eq!(qty_after, 70, "DeliverRequest must consume 30 Pyerite (3 batches of 10), reducing 100 to 70");

    let isk_before_freelance = ctx.db.with_snapshot(|s| s.characters[&90000001].balance.0);

    // 4. RedeemRewardRequest -> exact 300,000 ISK (3 batches * 100,000 ISK) + FreelanceReward journal entry
    let redeem_req = make_public_req("eve_public.freelance.project.api.RedeemRewardRequest", commit_bytes.clone(), now);
    let redeem_rsp = ctx.quasar_gateway.handle_public_request(&sess, &redeem_req, None, false, now);
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    assert!(redeem_rsp.payload.unwrap().type_url.contains("RedeemRewardResponse"));

    let isk_after_freelance = ctx.db.with_snapshot(|s| s.characters[&90000001].balance.0);
    assert_eq!(isk_after_freelance - isk_before_freelance, eve_db::IskCents::from_isk(300_000).0);
    let has_fl_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|e| e.character_id == 90000001 && e.entry_type == "FreelanceReward" && e.amount == eve_db::IskCents::from_isk(300_000))
    });
    assert!(has_fl_journal, "RedeemRewardRequest must append FreelanceReward journal entry");

    // 5. GetLimits, GetStats, GetAllActive
    let lim_req = make_public_req("eve_public.freelance.project.api.GetCreationLimitsRequest", Vec::new(), now);
    let lim_rsp = ctx.quasar_gateway.handle_public_request(&sess, &lim_req, None, false, now);
    assert_eq!(lim_rsp.status_code, STATUS_OK);
    let lim_wm = eve_proto::quasar::WireMessage::decode(&lim_rsp.payload.unwrap().value).unwrap();
    assert_eq!(lim_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(100));

    let stats_req = make_public_req("eve_public.freelance.project.api.GetStatsRequest", commit_bytes, now);
    let stats_rsp = ctx.quasar_gateway.handle_public_request(&sess, &stats_req, None, false, now);
    assert_eq!(stats_rsp.status_code, STATUS_OK);
    assert!(!stats_rsp.payload.unwrap().value.is_empty());

    // 6. Legacy FreelanceProjectRecord JSON lacking required_type_id/batch_size deserializes with defaults (34, 5)
    let legacy_json = r#"{"uuid":[1,2,3,4],"owner_corp_id":98000001,"creator_char_id":90000001,"name":"Legacy","description":"Old","career":1,"deadline":0,"created":0,"target_progress":100,"current_progress":0,"state":1,"committed_participants":[],"contributors":{},"reward_isk":1000}"#;
    let legacy_rec: eve_server::services::quasar_social_freelance::FreelanceProjectRecord =
        serde_json::from_str(legacy_json).expect("legacy FreelanceProjectRecord must deserialize");
    assert_eq!(legacy_rec.required_type_id, 34);
    assert_eq!(legacy_rec.batch_size, 5);
}

#[tokio::test]
async fn test_quasar_mail_calendar_contacts_standings_complete() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(107, 1, 90000001, 98000001);

    for (url, payload) in [
        ("eve.mail.cspa.GetRequest", Vec::new()),
        ("eve.calendar.personal_event.GetRequest", encode_seq_id(1, 1)),
        ("eve.calendar.corporation_event.GetRequest", encode_seq_id(1, 1)),
        ("eve.calendar.alliance_event.GetRequest", encode_seq_id(1, 1)),
        ("eve.contact.label.GetForCharacterRequest", Vec::new()),
        ("eve.standing.api.IncreaseCommanded", Vec::new()),
        ("eve.standing.api.DecreaseCommanded", Vec::new()),
    ] {
        let req = make_public_req(url, payload, now);
        let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
        assert_eq!(rsp.status_code, STATUS_OK, "failed: {url}");
    }

    let mut mail_bytes = Vec::new();
    encode_string_field(3, "Operation Orders", &mut mail_bytes);
    encode_string_field(4, "Fleet form up in Jita at 20:00", &mut mail_bytes);
    let mail_req = make_public_req("eve.mail.CCPSendMailRequest", mail_bytes, now);
    let mail_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mail_req, None, false, now);
    assert_eq!(mail_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_wars_tags_bookmarks_fleet_medal_security_complete() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(108, 1, 90000001, 98000001);

    for (url, payload, want) in [
        ("eve.war.GetRequest", encode_seq_id(1, 1001), "GetResponse"),
        ("eve.war.GetAllRequest", Vec::new(), "GetAllResponse"),
        ("eve.war.GetDestructionStatisticsRequest", encode_seq_id(1, 1001), "GetDestructionStatisticsResponse"),
        ("eve.war.GetAlliesRequest", encode_seq_id(1, 1001), "GetAlliesResponse"),
        ("eve.war.GetKillMailsRequest", encode_seq_id(1, 1001), "GetKillMailsResponse"),
        ("eve.tag.api.RegisterRequest", Vec::new(), "RegisterResponse"),
        ("eve.tag.api.ValidateRequest", Vec::new(), "ValidateResponse"),
        ("eve.tag.api.ListRequest", Vec::new(), "ListResponse"),
    ] {
        let req = make_public_req(url, payload, now);
        let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
        assert_eq!(rsp.status_code, STATUS_OK, "failed: {url}");
        assert!(rsp.payload.unwrap().type_url.contains(want));
    }
}
