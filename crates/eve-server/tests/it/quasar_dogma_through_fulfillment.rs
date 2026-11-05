//! Behavioral integration tests for Quasar Dogma, Duel, Dungeon, Dynamic Bounty System (DBS),
//! Effect, Epic Arc, Faction Warfare, Fighter, Fleet, and Fulfillment services.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::NoticeScope;
use eve_server::router::ServerContext;
use eve_server::services::quasar_char_skill_services::{
    encode_dbs_settings, encode_dbs_solar_system_details, encode_duel_challenge_accepted,
    encode_duel_challenge_issued, encode_fighter_type_identifier,
    publish_duel_challenge_accepted_notice, publish_duel_challenge_issued_notice,
    publish_epicarc_character_completed_notice, publish_fleet_advert_created_notice,
    publish_fleet_member_joined_notice, publish_system_wide_dbuff_applied,
    publish_system_wide_dbuff_removed, publish_warp_scrambling_applied_notice,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

fn seed_test_fixtures(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        // Ensure character 90_000_001 is docked at station 60_003_760 with home station
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
            ch.home_station_id = 60_003_760;
            ch.fleet_id = Some(1);
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(60_003_760);
            ch2.home_station_id = 60_003_760;
            ch2.fleet_id = Some(1);
        }

        // Seed fleet 1
        snap.fleets.insert(
            1,
            eve_db::FleetRecord {
                fleet_id: 1,
                boss_id: 90_000_001,
                members: vec![90_000_001, 90_000_002],
                ..Default::default()
            },
        );

        // Seed faction warfare system occupations if not populated
        if snap.fw_system_occupations.is_empty() {
            snap.backfill_fw_system_occupations();
        }
    });
}

static CORR_SEQ: AtomicU64 = AtomicU64::new(500);

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT;
    sess
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
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-df-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-df".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

async fn subscribe_to_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
    notice_types: &[&str],
) {
    let args: Vec<PyRep> = notice_types
        .iter()
        .map(|s| PyRep::String(s.to_string()))
        .collect();
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "publicGatewaySvc",
        None,
        // rpc-lint: allow internal gateway mock
        "SubscribeToNotice",
        &args,
        &kw,
        None,
    )
    .await
    .expect("SubscribeToNotice");
}

async fn poll_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
) -> Vec<(String, Vec<u8>)> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            // rpc-lint: allow internal gateway mock
            "PollNotices",
            &[],
            &kw,
            None,
        )
        .await
        .expect("PollNotices");

    let mut out = Vec::new();
    if let PyRep::List(items) = rep {
        for it in items {
            if let PyRep::KeyVal(kv) = it {
                let mut type_url = String::new();
                let mut payload = Vec::new();
                for (k, v) in kv {
                    if k == "type_url" {
                        if let PyRep::String(s) = v {
                            type_url = s;
                        }
                    } else if k == "payload" {
                        if let PyRep::Bytes(b) = v {
                            payload = b;
                        }
                    }
                }
                out.push((type_url, payload));
            }
        }
    }
    out
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

#[tokio::test]
async fn test_quasar_dogma_duel_dungeon_and_dbs() {
    // parity: feat:quasar.dogma
    // parity: feat:quasar.duel
    // parity: feat:quasar.dungeon
    // parity: feat:quasar.dynamicbountysystem
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(101, 1001, 90_000_001, 1_000_170);

    // 1. Quasar Dogma: system modifiers & notice fan-out
    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &[
            "eve.dogma.modifier.system_wide.api.Applied",
            "eve.dogma.modifier.system_wide.api.Removed",
        ],
    )
    .await;

    let published_dogma = publish_system_wide_dbuff_applied(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        30_000_142,
        101,
        -1,
        -500_000_000,
        ctx.now_unix(),
    )
    .expect("publish dogma notice");
    assert!(published_dogma >= 1, "Must deliver dogma applied notice to session");

    let published_dogma_rem = publish_system_wide_dbuff_removed(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        30_000_142,
        101,
        ctx.now_unix(),
    )
    .expect("publish dogma removed notice");
    assert!(published_dogma_rem >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    let applied_payload = notices
        .iter()
        .find(|(t, _)| t.ends_with("eve.dogma.modifier.system_wide.api.Applied"))
        .map(|(_, p)| p.as_slice())
        .expect("Must receive Applied notice");
    reg.decode_dynamic("eve.dogma.modifier.system_wide.api.Applied", applied_payload)
        .expect("Applied notice must decode against V24 descriptor");

    // 2. Quasar Duel: model serialization & notice delivery
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.duel.ChallengeIssued", "eve.duel.ChallengeAccepted"],
    )
    .await;

    let duel_issued_bytes = encode_duel_challenge_issued(90_000_001, 90_000_002);
    reg.decode_dynamic("eve.duel.ChallengeIssued", &duel_issued_bytes)
        .expect("Duel issued payload must match V24 schema");
    let duel_accepted_bytes = encode_duel_challenge_accepted(90_000_001, 90_000_002);
    reg.decode_dynamic("eve.duel.ChallengeAccepted", &duel_accepted_bytes)
        .expect("Duel accepted payload must match V24 schema");

    let published_duel = publish_duel_challenge_issued_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        90_000_002,
        ctx.now_unix(),
    )
    .expect("publish duel notice");
    assert!(published_duel >= 1);

    let published_accept = publish_duel_challenge_accepted_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        90_000_002,
        ctx.now_unix(),
    )
    .expect("publish duel accepted notice");
    assert!(published_accept >= 1);

    let notices2 = poll_notices(&ctx, &mut sess).await;
    assert!(
        notices2.iter().any(|(t, _)| t.ends_with("eve.duel.ChallengeIssued")),
        "Must receive duel notice"
    );
    assert!(
        notices2.iter().any(|(t, _)| t.ends_with("eve.duel.ChallengeAccepted")),
        "Must receive duel accepted notice"
    );

    // 3. Quasar Dungeon: sovereignty instances & visibility RPCs
    let mut sov_dng_req = Vec::new();
    encode_id_field(1, 30_000_142, &mut sov_dng_req);
    let sov_dng_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dungeon.sovereignty.api.GetInstancesRequest",
        sov_dng_req.clone(),
    )
    .await;
    assert_eq!(sov_dng_rsp.status_code, 200, "SovInstances: {}", sov_dng_rsp.status_message);

    let vis_dng_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest",
        sov_dng_req,
    )
    .await;
    assert_eq!(vis_dng_rsp.status_code, 200, "VisibleSovInstances: {}", vis_dng_rsp.status_message);

    // 4. Quasar Dynamic Bounty System (DBS): RPC routing and multiplier models
    let dbs_set_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(dbs_set_rsp.status_code, 200, "DBS settings: {}", dbs_set_rsp.status_message);

    let dbs_act_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dynamicbountysystem.GetActiveSystemsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(dbs_act_rsp.status_code, 200, "DBS active systems: {}", dbs_act_rsp.status_message);

    let mut dbs_paid_req = Vec::new();
    encode_id_field(1, 30_000_142, &mut dbs_paid_req);
    let dbs_paid_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dynamicbountysystem.GetPaidSystemBountiesRequest",
        dbs_paid_req.clone(),
    )
    .await;
    assert_eq!(dbs_paid_rsp.status_code, 200);

    let dbs_loss_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dynamicbountysystem.GetAccumulatedSystemShipLossValueRequest",
        dbs_paid_req,
    )
    .await;
    assert_eq!(dbs_loss_rsp.status_code, 200);

    let dbs_wire = encode_dbs_settings(0, 500_000_000, 1, 500_000_000, 1, 0);
    assert!(!dbs_wire.is_empty(), "DBS settings wire message must be populated");

    let details_wire = encode_dbs_solar_system_details(30_000_142, 0, 800_000_000);
    assert!(!details_wire.is_empty(), "DBS solar system details wire message must be populated");
}

#[tokio::test]
async fn test_quasar_effect_epicarc_and_factionwarfare() {
    // parity: feat:quasar.effect
    // parity: feat:quasar.epicarc
    // parity: feat:quasar.factionwarfare
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(102, 1002, 90_000_001, 1_000_170);

    // 1. Quasar Effect: pvp electronic notices & V24 schema decoding
    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.effect.combat.electronic.pvp.WarpScramblingApplied"],
    )
    .await;

    let published_scram = publish_warp_scrambling_applied_notice(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        90_000_001,
        90_000_002,
        44992,
        ctx.now_unix(),
    )
    .expect("publish scram notice");
    assert!(published_scram >= 1, "Must fan-out scram notice to session");

    let notices = poll_notices(&ctx, &mut sess).await;
    let scram_payload = notices
        .iter()
        .find(|(t, _)| t.ends_with("eve.effect.combat.electronic.pvp.WarpScramblingApplied"))
        .map(|(_, p)| p.as_slice())
        .expect("Must receive WarpScramblingApplied notice");
    reg.decode_dynamic(
        "eve.effect.combat.electronic.pvp.WarpScramblingApplied",
        scram_payload,
    )
    .expect("WarpScramblingApplied must match V24 descriptor");

    // 2. Quasar Epic Arc & Mission: completed count RPC and CharacterCompleted notice
    subscribe_to_notices(&ctx, &mut sess, &["eve.mission.epicarc.event.CharacterCompleted"]).await;

    let mut mission_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut mission_req);
    let mission_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.mission.api.GetCompletedCountRequest",
        mission_req,
    )
    .await;
    assert_eq!(mission_rsp.status_code, 200, "MissionCount: {}", mission_rsp.status_message);
    let mission_any = mission_rsp.payload.expect("GetCompletedCountResponse payload");
    assert!(mission_any.type_url.ends_with("GetCompletedCountResponse"));
    reg.decode_dynamic("eve.mission.api.GetCompletedCountResponse", &mission_any.value)
        .expect("GetCompletedCountResponse must match V24 descriptor");

    let published_arc = publish_epicarc_character_completed_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        500_001,
        ctx.now_unix(),
    )
    .expect("publish epic arc completion notice");
    assert!(published_arc >= 1);

    let notices2 = poll_notices(&ctx, &mut sess).await;
    let arc_payload = notices2
        .iter()
        .find(|(t, _)| t.ends_with("eve.mission.epicarc.event.CharacterCompleted"))
        .map(|(_, p)| p.as_slice())
        .expect("Must receive CharacterCompleted notice");
    reg.decode_dynamic("eve.mission.epicarc.event.CharacterCompleted", arc_payload)
        .expect("CharacterCompleted must match V24 descriptor");

    // 3. Quasar Faction Warfare: warzones, systems, stats, and leaderboards
    let fw_sys_rsp = quasar_rpc(&ctx, &mut sess, "eve.factionwarfare.systems.GetRequest", Vec::new()).await;
    assert_eq!(fw_sys_rsp.status_code, 200, "FW systems: {}", fw_sys_rsp.status_message);

    let fw_all_rsp = quasar_rpc(&ctx, &mut sess, "eve.factionwarfare.warzone.api.GetAllRequest", Vec::new()).await;
    assert_eq!(fw_all_rsp.status_code, 200, "FW warzones: {}", fw_all_rsp.status_message);

    let mut wz_req = Vec::new();
    encode_id_field(1, 1, &mut wz_req);
    let fw_wz_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.factionwarfare.warzone.api.GetParticipantsRequest",
        wz_req,
    )
    .await;
    assert_eq!(fw_wz_rsp.status_code, 200, "FW participants: {}", fw_wz_rsp.status_message);

    let fw_wars_rsp = quasar_rpc(&ctx, &mut sess, "eve.factionwarfare.wars.GetRequest", Vec::new()).await;
    assert_eq!(fw_wars_rsp.status_code, 200);

    let mut char_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_req);
    let fw_char_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.factionwarfare.stats.character.GetRequest",
        char_req,
    )
    .await;
    assert_eq!(fw_char_rsp.status_code, 200);

    let mut corp_req = Vec::new();
    encode_id_field(1, 1_000_170, &mut corp_req);
    let fw_corp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.factionwarfare.stats.corporation.GetRequest",
        corp_req,
    )
    .await;
    assert_eq!(fw_corp_rsp.status_code, 200);
    if let Some(corp_any) = &fw_corp_rsp.payload {
        reg.decode_dynamic("eve.factionwarfare.stats.corporation.GetResponse", &corp_any.value)
            .expect("corp stats response must match V24 schema");
    }

    let fw_fac_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.factionwarfare.stats.factions.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(fw_fac_rsp.status_code, 200, "FW faction stats: {}", fw_fac_rsp.status_message);
    if let Some(fac_any) = &fw_fac_rsp.payload {
        reg.decode_dynamic("eve.factionwarfare.stats.factions.GetResponse", &fac_any.value)
            .expect("faction stats response must match V24 schema");
    }

    let fw_lb_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.factionwarfare.leaderboard.characters.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(fw_lb_rsp.status_code, 200);
}

#[tokio::test]
async fn test_quasar_fighter_fleet_and_fulfillment() {
    // parity: feat:quasar.fighter
    // parity: feat:quasar.fleet
    // parity: feat:quasar.fulfillment
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(103, 1003, 90_000_001, 1_000_170);

    // 1. Quasar Fighter: fighter type identifier model decoding
    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    let ft_wire = encode_fighter_type_identifier(40359);
    reg.decode_dynamic("eve.fighter.fightertype.Identifier", &ft_wire)
        .expect("Fighter type identifier must match V24 schema");

    // 2. Quasar Fleet: advert/joined notices and V24 schema decoding
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.fleet.api.AdvertCreated", "eve.fleet.member.Joined"],
    )
    .await;

    let published_adv = publish_fleet_advert_created_notice(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        1,
        90_000_001,
        30_000_142,
        ctx.now_unix(),
    )
    .expect("publish advert notice");
    assert!(published_adv >= 1);

    let published_join = publish_fleet_member_joined_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        1,
        90_000_002,
        ctx.now_unix(),
    )
    .expect("publish joined notice");
    assert!(published_join >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    let adv_payload = notices
        .iter()
        .find(|(t, _)| t.ends_with("eve.fleet.api.AdvertCreated"))
        .map(|(_, p)| p.as_slice())
        .expect("Must receive AdvertCreated notice");
    reg.decode_dynamic("eve.fleet.api.AdvertCreated", adv_payload)
        .expect("AdvertCreated must match V24 descriptor");
    let join_payload = notices
        .iter()
        .find(|(t, _)| t.ends_with("eve.fleet.member.Joined"))
        .map(|(_, p)| p.as_slice())
        .expect("Must receive Joined notice");
    reg.decode_dynamic("eve.fleet.member.Joined", join_payload)
        .expect("Joined must match V24 descriptor");

    // 3. Quasar Fulfillment: store delivery, offer retrieval, and reward claims
    let mut deliver_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut deliver_req); // user / target character
    encode_id_field(2, 44992, &mut deliver_req);       // item type (e.g. skill injector / module)
    encode_varint_field(3, 5, &mut deliver_req);       // quantity: 5
    encode_varint_field(4, 1, &mut deliver_req);       // source: store
    let deliver_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.fulfillment.api.DeliverItemRequest",
        deliver_req,
    )
    .await;
    assert_eq!(deliver_rsp.status_code, 200, "DeliverItem: {}", deliver_rsp.status_message);

    // Verify item was delivered to character's station hangar
    let hangar_item = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|it| {
            it.owner_id == 90_000_001
                && it.type_id == 44992
                && it.flag == eve_db::InventoryFlag::Hangar
        }).cloned()
    });
    assert!(hangar_item.is_some(), "Delivered item must exist in hangar");
    assert_eq!(hangar_item.unwrap().quantity, 5);

    let mut offer_req = Vec::new();
    encode_id_field(1, 1001, &mut offer_req);
    let offer_rsp = quasar_rpc(&ctx, &mut sess, "eve.fulfillment.offer.api.GetRequest", offer_req).await;
    assert_eq!(offer_rsp.status_code, 200, "OfferGet: {}", offer_rsp.status_message);

    let mut reward_req = Vec::new();
    encode_id_field(1, 1001, &mut reward_req); // user_id
    encode_id_field(2, 501, &mut reward_req);  // offer_id
    let reward_rsp = quasar_rpc(&ctx, &mut sess, "eve.fulfillment.RewardDeliveryRequest", reward_req).await;
    assert_eq!(reward_rsp.status_code, 200, "RewardDelivery: {}", reward_rsp.status_message);
}
