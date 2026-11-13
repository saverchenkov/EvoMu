use super::*;
use crate::quasar_gateway::NoticeScope;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

#[cfg(test)]
#[path = "quasar_char_skill_services_cov_edges_tests.rs"]
mod quasar_char_skill_services_cov_edges_tests;

#[test]
fn char_skill_service_encoders_notices_and_gateway_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let router = &ctx.quasar_gateway;
    let cid = 100_000_001_u64;
    let scope = NoticeScope::Character(cid as u32);

    assert!(publish_academy_borrowed_notice(router, scope.clone(), cid, 587, 900_001, 1_700_000_000).is_ok());
    assert!(publish_acl_membership_updated_notice(router, scope.clone(), 1, 2, 1_700_000_000).is_ok());
    assert!(publish_system_wide_dbuff_applied(router, scope.clone(), 30_000_142, 10, 1, 500, 1_700_000_000).is_ok());
    assert!(publish_system_wide_dbuff_removed(router, scope.clone(), 30_000_142, 10, 1_700_000_000).is_ok());
    assert!(publish_duel_challenge_issued_notice(router, scope.clone(), cid, cid + 1, 1_700_000_000).is_ok());
    assert!(publish_duel_challenge_accepted_notice(router, scope.clone(), cid, cid + 1, 1_700_000_000).is_ok());
    assert!(publish_warp_scrambling_applied_notice(router, scope.clone(), cid, cid + 1, 3242, 1_700_000_000).is_ok());
    assert!(publish_epicarc_character_completed_notice(router, scope.clone(), cid, 500_001, 1_700_000_000).is_ok());
    assert!(publish_fleet_advert_created_notice(router, scope.clone(), 1, cid, 30_000_142, 1_700_000_000).is_ok());
    assert!(publish_fleet_member_joined_notice(router, scope, 1, cid, 1_700_000_000).is_ok());

    assert!(!encode_dbs_setting_value(1, 500_000_000).is_empty());
    assert!(!encode_dbs_settings(0, 500_000_000, 2, 0, 1, 100_000_000).is_empty());
    assert!(!encode_dbs_solar_system_details(30_000_142, 1, 200_000_000).is_empty());
    assert!(!encode_fighter_type_identifier(23055).is_empty());
    assert!(!encode_fighter_squad_state(1, 23055, 9, &[1, 2]).is_empty());
    assert!(!encode_epicarc_progress(1, 3, true).is_empty());

    let raw_wire = eve_proto::quasar::WireMessage {
        fields: vec![
            eve_proto::quasar::WireField {
                field_number: 1,
                value: eve_proto::quasar::WireValue::LengthDelimited(
                    eve_proto::pb::message("eve.solarsystem.Identifier")
                        .set("sequential", 30_000_142i64)
                        .must_encode(),
                ),
            },
            eve_proto::quasar::WireField {
                field_number: 2,
                value: eve_proto::quasar::WireValue::Varint(99),
            },
        ],
    }
    .encode();
    let decoded = decode_wire_dynamic("test.Msg", &raw_wire);
    assert_eq!(extract_u64_checked(&decoded, 1).unwrap(), Some(30_000_142));
    assert_eq!(extract_u64_checked(&decoded, 2).unwrap(), Some(99));

    // Nested depth >= 2 submessage and printable ASCII preservation
    let inner = eve_proto::quasar::WireMessage {
        fields: vec![eve_proto::quasar::WireField {
            field_number: 3,
            value: eve_proto::quasar::WireValue::Varint(777),
        }],
    }
    .encode();
    let mid = eve_proto::quasar::WireMessage {
        fields: vec![eve_proto::quasar::WireField {
            field_number: 2,
            value: eve_proto::quasar::WireValue::LengthDelimited(inner),
        }],
    }
    .encode();
    let outer = eve_proto::quasar::WireMessage {
        fields: vec![
            eve_proto::quasar::WireField {
                field_number: 1,
                value: eve_proto::quasar::WireValue::LengthDelimited(mid),
            },
            eve_proto::quasar::WireField {
                field_number: 4,
                value: eve_proto::quasar::WireValue::LengthDelimited(b"8@".to_vec()),
            },
        ],
    }
    .encode();
    let deep = decode_wire_dynamic("test.DeepMsg", &outer);
    let Some(DynamicValue::Message(mid_msg)) = deep.get(1) else {
        panic!("expected mid message");
    };
    let Some(DynamicValue::Message(inner_msg)) = mid_msg.get(2) else {
        panic!("expected depth-2 inner message");
    };
    assert_eq!(inner_msg.get(3), Some(&DynamicValue::Uint64(777)));
    assert_eq!(deep.get(4), Some(&DynamicValue::Bytes(b"8@".to_vec())));

    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    let empty_msg = DynamicQuasarMessage::new("Empty");
    assert!(handlers["eve.ship.api.CanJumpStarterSpaceShipCasterRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.starterspace.api.CanJumpShipCasterRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.stargate.lock.api.GetRestrictedSystemsRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.sovereignty.api.GetVersionManifestRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.sovereignty.api.GetAllClaimsRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.ancestry.GetAllRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.factionwarfare.stats.factions.GetRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.fulfillment.RewardDeliveryRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.mission.api.GetCompletedCountRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.monolith.GetClusterInfoRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.monolith.character.location.GetRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.monolith.character.logonstatus.GetRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.monolith.legacy.json.Request"](&admin_ctx, &dummy_any, &empty_msg).is_ok());

    let mut sys_msg = DynamicQuasarMessage::new("Sys");
    sys_msg.fields.insert(1, DynamicValue::Uint32(30_000_142));
    assert!(handlers["eve.sovereignty.api.GetClaimRequest"](&admin_ctx, &dummy_any, &sys_msg).is_ok());
    assert!(handlers["eve.sovereignty.activitydefensemodifier.api.GetLevelRequest"](&admin_ctx, &dummy_any, &sys_msg).is_ok());

    let mut acl_msg = DynamicQuasarMessage::new("Acl");
    acl_msg.fields.insert(1, DynamicValue::Uint32(1));
    acl_msg.fields.insert(2, DynamicValue::Uint64(90_000_001));
    assert!(handlers["eve.acl.api.GetRequest"](&admin_ctx, &dummy_any, &acl_msg).is_ok());
    assert!(handlers["eve.acl.api.CanManageRequest"](&admin_ctx, &dummy_any, &acl_msg).is_ok());

    let mut char_msg = DynamicQuasarMessage::new("Char");
    char_msg.fields.insert(1, DynamicValue::Uint64(90_000_001));
    assert!(handlers["eve.acl.api.GetManagedRequest"](&admin_ctx, &dummy_any, &char_msg).is_ok());
    assert!(handlers["eve.factionwarfare.stats.corporation.GetRequest"](&admin_ctx, &dummy_any, &char_msg).is_ok());
    assert!(handlers["eve.fulfillment.offer.api.GetRequest"](&admin_ctx, &dummy_any, &char_msg).is_ok());

    let mut vj_msg = DynamicQuasarMessage::new("ValidateJump");
    vj_msg.fields.insert(1, DynamicValue::Uint64(cid));
    vj_msg.fields.insert(2, DynamicValue::Uint32(30_000_142));
    vj_msg.fields.insert(3, DynamicValue::Uint64(50_000_001));
    assert!(handlers["eve.stargate.lock.api.ValidateJumpRequest"](&admin_ctx, &dummy_any, &vj_msg).is_ok());
}

#[test]
fn char_skill_builders_decode_strictly_with_descriptors() {
    use eve_proto::quasar::{
        AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
        QuasarDescriptorRegistry,
    };

    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
    let roundtrip = |type_name: &str, wire: &[u8]| {
        let decoded = reg
            .decode_dynamic(type_name, wire)
            .unwrap_or_else(|e| panic!("decode {type_name}: {e}"));
        let reencoded = reg
            .encode_dynamic(&decoded)
            .unwrap_or_else(|e| panic!("encode {type_name}: {e}"));
        let decoded2 = reg
            .decode_dynamic(type_name, &reencoded)
            .unwrap_or_else(|e| panic!("redecode {type_name}: {e}"));
        assert_eq!(decoded, decoded2, "roundtrip mismatch for {type_name}");
    };

    // Standalone helper encoders backed by descriptors
    roundtrip("eve.duel.ChallengeIssued", &encode_duel_challenge_issued(100, 200));
    roundtrip("eve.duel.ChallengeAccepted", &encode_duel_challenge_accepted(100, 200));
    roundtrip("eve.dynamicbountysystem.SettingValue", &encode_dbs_setting_value(1, 500_000_000));
    roundtrip(
        "eve.dynamicbountysystem.Settings",
        &encode_dbs_settings(0, 500_000_000, 2, 0, 1, 0),
    );
    roundtrip(
        "eve.dynamicbountysystem.SolarSystemDetails",
        &encode_dbs_solar_system_details(30_000_142, 0, 500_000_000),
    );
    roundtrip("eve.fighter.fightertype.Identifier", &encode_fighter_type_identifier(23055));

    // Handlers
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 100_000_001_u32;
    let sys_id = 30_000_142_u32;
    let zarzakh = 30_100_000_u32;
    let struct_id = 1_000_000_991_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut chr = eve_db::CharacterRecord::default();
        chr.character_id = i64::from(cid);
        chr.name = "Tester".to_string();
        chr.solar_system_id = sys_id;
        chr.station_id = Some(60_003_760);
        chr.corporation_id = 1_000_009;
        s.characters.insert(i64::from(cid), chr);

        s.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Test Astrahus".to_string(),
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        s.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                sov_alliance_id: Some(99_000_001),
                military_index: 5,
                industrial_index: 5,
                strategic_index: 5,
                ..Default::default()
            },
        );
        s.sov_runtime.sov_hubs.insert(struct_id, Default::default());
        s.sov_runtime.sov_claimed_since_unix.insert(sys_id, 1_700_000_000);
        s.zarzakh_gate_locks.insert(i64::from(cid), (50_000_001, 1_700_003_600));
    });

    let router = &ctx.quasar_gateway;
    let handlers = router.handlers.read().unwrap();

    let auth_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();

    let id_sub = |val: u64| {
        let mut sub = DynamicQuasarMessage::new("Identifier");
        sub.set(1, DynamicValue::Uint64(val));
        DynamicValue::Message(Box::new(sub))
    };

    let mut req_sys = DynamicQuasarMessage::new("ReqSys");
    req_sys.set(1, id_sub(sys_id as u64));

    let mut req_char_sys = DynamicQuasarMessage::new("ReqCharSys");
    req_char_sys.set(1, id_sub(cid as u64));
    req_char_sys.set(2, id_sub(zarzakh as u64));

    let mut req_validate = DynamicQuasarMessage::new("ValidateJump");
    req_validate.set(1, id_sub(cid as u64));
    req_validate.set(2, id_sub(zarzakh as u64));
    req_validate.set(3, id_sub(50_000_002));

    let mut req_validate_free = req_validate.clone();
    req_validate_free.set(2, id_sub(sys_id as u64));

    let mut req_struct = DynamicQuasarMessage::new("StructReq");
    req_struct.set(1, id_sub(struct_id as u64));

    let mut req_unclaimed = DynamicQuasarMessage::new("UnclaimedReq");
    req_unclaimed.set(1, id_sub(30_000_144));

    let mut req_agent = DynamicQuasarMessage::new("AgentReq");
    req_agent.set(1, id_sub(3_008_416));

    let mut req_acl = DynamicQuasarMessage::new("AclReq");
    req_acl.set(1, DynamicValue::Uint32(1));
    req_acl.set(2, id_sub(cid as u64));

    let mut req_corp = DynamicQuasarMessage::new("CorpReq");
    req_corp.set(1, id_sub(1_000_009));

    let mut req_offer = DynamicQuasarMessage::new("OfferReq");
    req_offer.set(1, id_sub(501));

    let mut req_char = DynamicQuasarMessage::new("CharReq");
    req_char.set(1, id_sub(cid as u64));

    let mut req_json = DynamicQuasarMessage::new("JsonReq");
    req_json.set(2, DynamicValue::String("{}".to_string()));

    let empty_req = DynamicQuasarMessage::new("Empty");

    let cases: &[(&str, &str, &DynamicQuasarMessage)] = &[
        ("eve.ship.api.CanJumpStarterSpaceShipCasterRequest", "eve.ship.api.CanJumpStarterSpaceShipCasterResponse", &empty_req),
        ("eve.starterspace.api.CanJumpShipCasterRequest", "eve.starterspace.api.CanJumpShipCasterResponse", &empty_req),
        ("eve.structure.api.GetRequest", "eve.structure.api.GetResponse", &req_struct),
        ("eve.stargate.lock.api.GetRequest", "eve.stargate.lock.api.GetResponse", &req_char_sys),
        ("eve.stargate.lock.api.GetRestrictedSystemsRequest", "eve.stargate.lock.api.GetRestrictedSystemsResponse", &empty_req),
        ("eve.stargate.lock.api.ValidateJumpRequest", "eve.stargate.lock.api.ValidateJumpResponse", &req_validate),
        ("eve.stargate.lock.api.ValidateJumpRequest", "eve.stargate.lock.api.ValidateJumpResponse", &req_validate_free),
        ("eve.sovereignty.api.GetClaimRequest", "eve.sovereignty.api.GetClaimResponse", &req_sys),
        ("eve.sovereignty.api.GetClaimRequest", "eve.sovereignty.api.GetClaimResponse", &req_unclaimed),
        ("eve.sovereignty.api.GetVersionManifestRequest", "eve.sovereignty.api.GetVersionManifestResponse", &empty_req),
        ("eve.sovereignty.api.GetAllClaimsRequest", "eve.sovereignty.api.GetAllClaimsResponse", &empty_req),
        ("eve.sovereignty.activitydefensemodifier.api.GetLevelRequest", "eve.sovereignty.activitydefensemodifier.api.GetLevelResponse", &req_sys),
        ("eve.ancestry.GetAllRequest", "eve.ancestry.GetAllResponse", &empty_req),
        ("eve.agent.GetRequest", "eve.agent.GetResponse", &req_agent),
        ("eve.agent.SearchRequest", "eve.agent.SearchResponse", &empty_req),
        ("eve.acl.api.GetRequest", "eve.acl.api.GetResponse", &req_acl),
        ("eve.acl.api.CanManageRequest", "eve.acl.api.CanManageResponse", &req_acl),
        ("eve.acl.api.GetManagedRequest", "eve.acl.api.GetManagedResponse", &req_char),
        ("eve.factionwarfare.stats.corporation.GetRequest", "eve.factionwarfare.stats.corporation.GetResponse", &req_corp),
        ("eve.factionwarfare.stats.factions.GetRequest", "eve.factionwarfare.stats.factions.GetResponse", &empty_req),
        ("eve.fulfillment.offer.api.GetRequest", "eve.fulfillment.offer.api.GetResponse", &req_offer),
        ("eve.fulfillment.RewardDeliveryRequest", "eve.fulfillment.RewardDeliveryResponse", &empty_req),
        ("eve.mission.api.GetCompletedCountRequest", "eve.mission.api.GetCompletedCountResponse", &req_char),
        ("eve.monolith.GetClusterInfoRequest", "eve.monolith.GetClusterInfoResponse", &empty_req),
        ("eve.monolith.character.location.GetRequest", "eve.monolith.character.location.GetResponse", &req_char),
        ("eve.monolith.character.logonstatus.GetRequest", "eve.monolith.character.logonstatus.GetResponse", &empty_req),
        ("eve.monolith.legacy.json.Request", "eve.monolith.legacy.json.Response", &req_json),
    ];

    for (req_name, resp_name, req) in cases {
        let resp = handlers[*req_name](&auth_ctx, &dummy_any, req)
            .unwrap_or_else(|e| panic!("handler {req_name} failed: {e:?}"));
        roundtrip(resp_name, &resp.value);
    }

    // Also test in_space and structure variants of monolith.character.location.GetResponse
    ctx.db.with_mut_snapshot(|s| {
        let chr = s.characters.get_mut(&i64::from(cid)).unwrap();
        chr.station_id = None;
        chr.structure_id = Some(struct_id);
    });
    let resp_struct = handlers["eve.monolith.character.location.GetRequest"](&auth_ctx, &dummy_any, &req_char).unwrap();
    roundtrip("eve.monolith.character.location.GetResponse", &resp_struct.value);

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&i64::from(cid)).unwrap().structure_id = None;
    });
    let resp_space = handlers["eve.monolith.character.location.GetRequest"](&auth_ctx, &dummy_any, &req_char).unwrap();
    roundtrip("eve.monolith.character.location.GetResponse", &resp_space.value);

    // Notice publishers
    let mut sess = eve_machonet::SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(cid.into());
    let mut rx = router.subscribe_session(&sess, Vec::new(), 1_700_000_000);
    let scope = NoticeScope::Broadcast;
    let now = 1_700_000_000;

    publish_academy_borrowed_notice(router, scope.clone(), cid as u64, 587, 900_001, now).unwrap();
    publish_acl_membership_updated_notice(router, scope.clone(), 1, 2, now).unwrap();
    publish_system_wide_dbuff_applied(router, scope.clone(), sys_id as u64, 42, 1, 500_000_000, now).unwrap();
    publish_system_wide_dbuff_removed(router, scope.clone(), sys_id as u64, 42, now).unwrap();
    publish_duel_challenge_issued_notice(router, scope.clone(), 100, 200, now).unwrap();
    publish_duel_challenge_accepted_notice(router, scope.clone(), 100, 200, now).unwrap();
    publish_warp_scrambling_applied_notice(router, scope.clone(), 100, 200, 3242, now).unwrap();
    publish_epicarc_character_completed_notice(router, scope.clone(), cid as u64, 500_001, now).unwrap();
    publish_fleet_advert_created_notice(router, scope.clone(), 700, cid as u64, sys_id as u64, now).unwrap();
    publish_fleet_member_joined_notice(router, scope, 700, cid as u64, now).unwrap();

    let mut seen = 0usize;
    while let Ok(notice) = rx.try_recv() {
        let payload = notice.payload.as_ref().expect("notice payload");
        let short_type = payload
            .type_url
            .rsplit('/')
            .next()
            .unwrap_or(&payload.type_url);
        roundtrip(short_type, &payload.value);
        seen += 1;
    }
    assert_eq!(seen, 10);
}

#[cfg(test)]
#[path = "quasar_char_skill_services_extra_unit_tests.rs"]
mod quasar_char_skill_services_extra_unit_tests;
