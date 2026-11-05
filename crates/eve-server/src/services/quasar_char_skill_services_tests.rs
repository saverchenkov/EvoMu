use super::*;
use crate::quasar_gateway::NoticeScope;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

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

    let mut raw_wire = Vec::new();
    encode_id_msg(1, 30_000_142, &mut raw_wire);
    encode_varint_field(2, 99, &mut raw_wire);
    let decoded = decode_wire_dynamic("test.Msg", &raw_wire);
    assert_eq!(extract_u64_checked(&decoded, 1).unwrap(), Some(30_000_142));
    assert_eq!(extract_u64_checked(&decoded, 2).unwrap(), Some(99));

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
