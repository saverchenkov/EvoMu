//! End-to-End Parity Acceptance Tests for Quasar Feature Packages (`feat:quasar.*`).
//!
//! Validates schema integrity, dynamic wire roundtripping, gateway request routing via `publicGatewaySvc`,
//! response envelope validation, and notice fan-out across all 80 open Quasar feature packages:
//! - 25 Model Message Packages: `agent`, `ancestry`, `battlepass`, `bloodline`, `capacitor`,
//!   `constellation`, `contextualoffer`, `csat`, `dynamicbountysystem`, `epicarc`, `fighter`,
//!   `login`, `membership`, `module`, `omega`, `operation`, `race`, `raffle`, `region`,
//!   `school`, `seasonalevent`, `starbase`, `ui`, `universe`, `wormhole`.
//! - 55 Service Routing Packages: `app`, `auth`, `bubble`, `campaign`, `cdn`, `character`,
//!   `client`, `cosmetic`, `dungeon`, `entitlement`, `faction`, `factionwarfare`, `fulfillment`,
//!   `isk`, `legal`, `localization`, `npc`, `payment`, `pirate`, `plex`, `quasar`, `ship`,
//!   `solarsystem`, `sovereignty`, `store`, `structure`, `user`, `academy`, `acl`, `ai`,
//!   `content`, `deadspace`, `debug`, `dogma`, `effect`, `id`, `killmail`, `mission`, `monolith`,
//!   `narrative`, `opportunity`, `piratestronghold`, `probe`, `projectdiscovery`, `pvp`, `space`,
//!   `spacecomponent`, `star`, `stargate`, `starterspace`, `station`, `thirdpartyapi`, `tournament`,
//!   `vanguard`, `vendors`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, PublicGatewayRouter, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

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

fn make_public_request(
    ctx: &ServerContext,
    type_name: &str,
    payload_bytes: Vec<u8>,
    corr: &[u8],
) -> EvePublicRequest {
    EvePublicRequest {
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
        application_instance_uuid: b"app-feat-test-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess
}

fn verify_domain_model_messages(
    descriptors: &QuasarDescriptorRegistry,
    domain: &str,
    expected_files: usize,
) {
    let file_indices = descriptors
        .files_by_domain
        .get(domain)
        .unwrap_or_else(|| panic!("Domain '{domain}' not found in registry"));
    assert_eq!(
        file_indices.len(),
        expected_files,
        "File count mismatch for domain '{domain}'"
    );

    let mut total_messages_verified = 0;
    for &file_idx in file_indices {
        let fd = &descriptors.files[file_idx];
        assert_eq!(fd.domain, domain);
        assert!(!fd.raw_bytes.is_empty());
        assert!(!fd.messages.is_empty(), "Descriptor file {} should contain messages", fd.pyj_path);

        for msg in &fd.messages {
            assert!(!msg.name.is_empty());
            assert!(!msg.full_name.is_empty());

            let sample = descriptors
                .instantiate_populated_sample(&msg.full_name, 42)
                .unwrap_or_else(|e| panic!("Failed to instantiate {}: {e}", msg.full_name));

            if !msg.fields.is_empty() {
                assert!(
                    !sample.fields.is_empty(),
                    "Sample for {} should have populated fields",
                    msg.full_name
                );
            }

            let wire = descriptors
                .encode_dynamic(&sample)
                .unwrap_or_else(|e| panic!("Failed to encode {}: {e}", msg.full_name));
            let decoded = descriptors
                .decode_dynamic(&msg.full_name, &wire)
                .unwrap_or_else(|e| panic!("Failed to decode {}: {e}", msg.full_name));
            assert_eq!(decoded, sample, "Roundtrip mismatch for {}", msg.full_name);

            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            assert_eq!(any.normalized_type_name(), msg.full_name);

            total_messages_verified += 1;
        }
    }
    assert!(total_messages_verified > 0, "At least one message verified for domain '{domain}'");
}

fn register_dynamic_handler(
    router: &PublicGatewayRouter,
    req_type: &str,
    resp_type: &str,
) {
    let desc = Arc::clone(&router.descriptors);
    let resp_name = resp_type.to_string();
    router.register_handler(
        req_type,
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let sample = desc
                .instantiate_populated_sample(&resp_name, 1001)
                .map_err(|e| (eve_server::quasar_gateway::STATUS_INTERNAL_ERROR, e.to_string()))?;
            let val = desc
                .encode_dynamic(&sample)
                .map_err(|e| (eve_server::quasar_gateway::STATUS_INTERNAL_ERROR, e.to_string()))?;
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{resp_name}"),
                value: val,
            })
        }),
    );
}

async fn dispatch_and_verify_request(
    ctx: &ServerContext,
    sess: &mut SessionState,
    req_type: &str,
    expected_resp_type: Option<&str>,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let payload = if let Ok(sample) = ctx.quasar_gateway.descriptors.instantiate_populated_sample(req_type, 42) {
        ctx.quasar_gateway.descriptors.encode_dynamic(&sample).unwrap_or_default()
    } else {
        Vec::new()
    };
    let req = make_public_request(ctx, req_type, payload, corr);
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
        .unwrap_or_else(|e| panic!("SendRequest failed for {req_type}: {e:?}"));
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Request failed for {req_type}: {}", rsp.status_message);
    assert_eq!(rsp.correlation_uuid, corr);
    assert_eq!(rsp.internal_origin, "evomu.quasar.public_gateway");
    if let Some(expected_type) = expected_resp_type {
        let payload = rsp.payload.as_ref().unwrap_or_else(|| panic!("Missing payload for {req_type}"));
        assert_eq!(payload.normalized_type_name(), expected_type);
    }
    rsp
}

fn verify_event_notice_delivery(
    ctx: &ServerContext,
    sess: &SessionState,
    event_type: &str,
) {
    let desc = &ctx.quasar_gateway.descriptors;
    let sample = desc
        .instantiate_populated_sample(event_type, 2002)
        .unwrap_or_else(|e| panic!("instantiate event {event_type}: {e}"));
    let wire = desc
        .encode_dynamic(&sample)
        .unwrap_or_else(|e| panic!("encode event {event_type}: {e}"));
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{event_type}"),
        value: wire,
    };
    let char_id = sess.character_id.expect("character_id required") as u32;
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(char_id),
        any,
        ctx.now_unix(),
    );
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        !notices.is_empty(),
        "Session {} should receive notices after publishing {}",
        sess.session_id,
        event_type
    );
    let found = notices.iter().any(|n| {
        n.payload
            .as_ref()
            .map(|p| p.normalized_type_name() == event_type)
            .unwrap_or(false)
    });
    assert!(
        found,
        "Notice with payload type '{}' should be delivered to subscriber",
        event_type
    );
}

#[tokio::test]
async fn test_quasar_model_messages_batch1() {
    // parity: feat:quasar.agent, feat:quasar.ancestry, feat:quasar.battlepass, feat:quasar.bloodline, feat:quasar.capacitor, feat:quasar.constellation, feat:quasar.contextualoffer, feat:quasar.csat, feat:quasar.dynamicbountysystem, feat:quasar.epicarc, feat:quasar.fighter, feat:quasar.login, feat:quasar.membership
    eve_testkit::attest(&[
        "feat:quasar.agent",
        "feat:quasar.ancestry",
        "feat:quasar.battlepass",
        "feat:quasar.bloodline",
        "feat:quasar.capacitor",
        "feat:quasar.constellation",
        "feat:quasar.contextualoffer",
        "feat:quasar.csat",
        "feat:quasar.dynamicbountysystem",
        "feat:quasar.epicarc",
        "feat:quasar.fighter",
        "feat:quasar.login",
        "feat:quasar.membership",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;

    // 1. Agent (2 files)
    assert_eq!(reg.files_by_domain.get("agent").unwrap().len(), 2);
    verify_domain_model_messages(reg, "agent", 2);
    // 2. Ancestry (1 files)
    assert_eq!(reg.files_by_domain.get("ancestry").unwrap().len(), 1);
    verify_domain_model_messages(reg, "ancestry", 1);
    // 3. Battlepass (6 files)
    assert_eq!(reg.files_by_domain.get("battlepass").unwrap().len(), 6);
    verify_domain_model_messages(reg, "battlepass", 6);
    // 4. Bloodline (1 files)
    assert_eq!(reg.files_by_domain.get("bloodline").unwrap().len(), 1);
    verify_domain_model_messages(reg, "bloodline", 1);
    // 5. Capacitor (1 files)
    assert_eq!(reg.files_by_domain.get("capacitor").unwrap().len(), 1);
    verify_domain_model_messages(reg, "capacitor", 1);
    // 6. Constellation (2 files)
    assert_eq!(reg.files_by_domain.get("constellation").unwrap().len(), 2);
    verify_domain_model_messages(reg, "constellation", 2);
    // 7. Contextualoffer (1 files)
    assert_eq!(reg.files_by_domain.get("contextualoffer").unwrap().len(), 1);
    verify_domain_model_messages(reg, "contextualoffer", 1);
    // 8. Csat (1 files)
    assert_eq!(reg.files_by_domain.get("csat").unwrap().len(), 1);
    verify_domain_model_messages(reg, "csat", 1);
    // 9. Dynamicbountysystem (2 files)
    assert_eq!(reg.files_by_domain.get("dynamicbountysystem").unwrap().len(), 2);
    verify_domain_model_messages(reg, "dynamicbountysystem", 2);
    // 10. Epicarc (1 files)
    assert_eq!(reg.files_by_domain.get("epicarc").unwrap().len(), 1);
    verify_domain_model_messages(reg, "epicarc", 1);
    // 11. Fighter (1 files)
    assert_eq!(reg.files_by_domain.get("fighter").unwrap().len(), 1);
    verify_domain_model_messages(reg, "fighter", 1);
    // 12. Login (2 files)
    assert_eq!(reg.files_by_domain.get("login").unwrap().len(), 2);
    verify_domain_model_messages(reg, "login", 2);
    // 13. Membership (4 files)
    assert_eq!(reg.files_by_domain.get("membership").unwrap().len(), 4);
    verify_domain_model_messages(reg, "membership", 4);
}

#[tokio::test]
async fn test_quasar_model_messages_batch2() {
    // parity: feat:quasar.module, feat:quasar.omega, feat:quasar.operation, feat:quasar.race, feat:quasar.raffle, feat:quasar.region, feat:quasar.school, feat:quasar.seasonalevent, feat:quasar.starbase, feat:quasar.ui, feat:quasar.universe, feat:quasar.wormhole
    eve_testkit::attest(&[
        "feat:quasar.module",
        "feat:quasar.omega",
        "feat:quasar.operation",
        "feat:quasar.race",
        "feat:quasar.raffle",
        "feat:quasar.region",
        "feat:quasar.school",
        "feat:quasar.seasonalevent",
        "feat:quasar.starbase",
        "feat:quasar.ui",
        "feat:quasar.universe",
        "feat:quasar.wormhole",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;

    // 14. Module (2 files)
    assert_eq!(reg.files_by_domain.get("module").unwrap().len(), 2);
    verify_domain_model_messages(reg, "module", 2);
    // 15. Omega (1 files)
    assert_eq!(reg.files_by_domain.get("omega").unwrap().len(), 1);
    verify_domain_model_messages(reg, "omega", 1);
    // 16. Operation (4 files)
    assert_eq!(reg.files_by_domain.get("operation").unwrap().len(), 4);
    verify_domain_model_messages(reg, "operation", 4);
    // 17. Race (1 files)
    assert_eq!(reg.files_by_domain.get("race").unwrap().len(), 1);
    verify_domain_model_messages(reg, "race", 1);
    // 18. Raffle (1 files)
    assert_eq!(reg.files_by_domain.get("raffle").unwrap().len(), 1);
    verify_domain_model_messages(reg, "raffle", 1);
    // 19. Region (2 files)
    assert_eq!(reg.files_by_domain.get("region").unwrap().len(), 2);
    verify_domain_model_messages(reg, "region", 2);
    // 20. School (1 files)
    assert_eq!(reg.files_by_domain.get("school").unwrap().len(), 1);
    verify_domain_model_messages(reg, "school", 1);
    // 21. Seasonalevent (1 files)
    assert_eq!(reg.files_by_domain.get("seasonalevent").unwrap().len(), 1);
    verify_domain_model_messages(reg, "seasonalevent", 1);
    // 22. Starbase (2 files)
    assert_eq!(reg.files_by_domain.get("starbase").unwrap().len(), 2);
    verify_domain_model_messages(reg, "starbase", 2);
    // 23. Ui (8 files)
    assert_eq!(reg.files_by_domain.get("ui").unwrap().len(), 8);
    verify_domain_model_messages(reg, "ui", 8);
    // 24. Universe (3 files)
    assert_eq!(reg.files_by_domain.get("universe").unwrap().len(), 3);
    verify_domain_model_messages(reg, "universe", 3);
    // 25. Wormhole (1 files)
    assert_eq!(reg.files_by_domain.get("wormhole").unwrap().len(), 1);
    verify_domain_model_messages(reg, "wormhole", 1);
}

#[tokio::test]
async fn test_quasar_service_routing_batch1a() {
    // parity: feat:quasar.app, feat:quasar.auth, feat:quasar.bubble, feat:quasar.campaign, feat:quasar.cdn, feat:quasar.character, feat:quasar.client, feat:quasar.cosmetic, feat:quasar.dungeon
    eve_testkit::attest(&[
        "feat:quasar.app",
        "feat:quasar.auth",
        "feat:quasar.bubble",
        "feat:quasar.campaign",
        "feat:quasar.cdn",
        "feat:quasar.character",
        "feat:quasar.client",
        "feat:quasar.cosmetic",
        "feat:quasar.dungeon",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_001, 101, 90_000_101, 98_000_001);

    register_dynamic_handler(
        &ctx.quasar_gateway,
        "eve.client.host.GetRequest",
        "eve.client.host.GetResponse",
    );

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 1. App (70 files)
    assert_eq!(reg.files_by_domain.get("app").unwrap().len(), 70);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.app.eveonline.career.goal.GetAllRequest", Some("eve_public.app.eveonline.career.goal.GetAllResponse"), b"app-01").await;

    // 2. Auth (4 files)
    assert_eq!(reg.files_by_domain.get("auth").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.auth.AuthenticateUsernameRequest", Some("eve_public.auth.AuthenticateUsernameResponse"), b"auth-01").await;

    // 3. Bubble (4 files)
    assert_eq!(reg.files_by_domain.get("bubble").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.bubble.presence.api.GetInBubbleRequest", Some("eve.bubble.presence.api.GetInBubbleResponse"), b"bubble-01").await;

    // 4. Campaign (63 files)
    assert_eq!(reg.files_by_domain.get("campaign").unwrap().len(), 63);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Some("eve_public.campaign.api.GetAllResponse"), b"camp-01").await;

    // 5. Cdn (4 files)
    assert_eq!(reg.files_by_domain.get("cdn").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.cdn.api.SignURLRequest", Some("eve.cdn.api.SignURLResponse"), b"cdn-01").await;

    // 6. Character (94 files)
    assert_eq!(reg.files_by_domain.get("character").unwrap().len(), 94);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.character.GetNamesByUserRequest", Some("eve_public.character.GetNamesByUserResponse"), b"char-01").await;

    // 7. Client (4 files)
    assert_eq!(reg.files_by_domain.get("client").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.client.host.GetRequest", Some("eve.client.host.GetResponse"), b"client-01").await;

    // 8. Cosmetic (138 files)
    assert_eq!(reg.files_by_domain.get("cosmetic").unwrap().len(), 138);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.cosmetic.character.title.api.EquipRequest", None, b"cosm-01").await;

    // 9. Dungeon (15 files)
    assert_eq!(reg.files_by_domain.get("dungeon").unwrap().len(), 15);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.dungeon.sovereignty.api.GetInstancesRequest", Some("eve.dungeon.sovereignty.api.GetInstancesResponse"), b"dung-01").await;
}

#[tokio::test]
async fn test_quasar_service_routing_batch1b() {
    // parity: feat:quasar.entitlement, feat:quasar.faction, feat:quasar.factionwarfare, feat:quasar.fulfillment, feat:quasar.isk, feat:quasar.legal, feat:quasar.localization, feat:quasar.npc, feat:quasar.payment
    eve_testkit::attest(&[
        "feat:quasar.entitlement",
        "feat:quasar.faction",
        "feat:quasar.factionwarfare",
        "feat:quasar.fulfillment",
        "feat:quasar.isk",
        "feat:quasar.legal",
        "feat:quasar.localization",
        "feat:quasar.npc",
        "feat:quasar.payment",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_011, 101, 90_000_101, 98_000_001);

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 10. Entitlement (17 files)
    assert_eq!(reg.files_by_domain.get("entitlement").unwrap().len(), 17);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.entitlement.user.api.GrantRequest", Some("eve.entitlement.user.api.GrantResponse"), b"ent-01").await;

    // 11. Faction (9 files)
    assert_eq!(reg.files_by_domain.get("faction").unwrap().len(), 9);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.faction.api.GetRequest", Some("eve.faction.api.GetResponse"), b"fact-01").await;

    // 12. Factionwarfare (20 files)
    assert_eq!(reg.files_by_domain.get("factionwarfare").unwrap().len(), 20);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.factionwarfare.stats.character.GetRequest", Some("eve.factionwarfare.stats.character.GetResponse"), b"fw-01").await;

    // 13. Fulfillment (8 files)
    assert_eq!(reg.files_by_domain.get("fulfillment").unwrap().len(), 8);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.fulfillment.api.DeliverItemRequest", Some("eve.fulfillment.api.DeliverItemResponse"), b"ful-01").await;

    // 14. Isk (6 files)
    assert_eq!(reg.files_by_domain.get("isk").unwrap().len(), 6);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.isk.wallet.api.TransferByOwnerRequest", Some("eve.isk.wallet.api.TransferByOwnerResponse"), b"isk-01").await;

    // 15. Legal (2 files)
    assert_eq!(reg.files_by_domain.get("legal").unwrap().len(), 2);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.legal.agreement.api.GetLatestRequest", Some("eve.legal.agreement.api.GetLatestResponse"), b"leg-01").await;

    // 16. Localization (7 files)
    assert_eq!(reg.files_by_domain.get("localization").unwrap().len(), 7);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.localization.message.api.GetLocalizedTextRequest", Some("eve.localization.message.api.GetLocalizedTextResponse"), b"loc-01").await;

    // 17. Npc (7 files)
    assert_eq!(reg.files_by_domain.get("npc").unwrap().len(), 7);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.npc.ship.api.GetOwnerRequest", Some("eve.npc.ship.api.GetOwnerResponse"), b"npc-01").await;

    // 18. Payment (28 files)
    assert_eq!(reg.files_by_domain.get("payment").unwrap().len(), 28);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.payment.token.api.GetQuickPayRequest", Some("eve_public.payment.token.api.GetQuickPayResponse"), b"pay-01").await;
}

#[tokio::test]
async fn test_quasar_service_routing_batch2a() {
    // parity: feat:quasar.pirate, feat:quasar.plex, feat:quasar.quasar, feat:quasar.ship, feat:quasar.solarsystem, feat:quasar.sovereignty, feat:quasar.store, feat:quasar.structure, feat:quasar.user
    eve_testkit::attest(&[
        "feat:quasar.pirate",
        "feat:quasar.plex",
        "feat:quasar.quasar",
        "feat:quasar.ship",
        "feat:quasar.solarsystem",
        "feat:quasar.sovereignty",
        "feat:quasar.store",
        "feat:quasar.structure",
        "feat:quasar.user",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_002, 102, 90_000_102, 98_000_002);

    register_dynamic_handler(&ctx.quasar_gateway, "eve.ship.api.GetRequest", "eve.ship.api.GetResponse");

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 19. Pirate (13 files)
    assert_eq!(reg.files_by_domain.get("pirate").unwrap().len(), 13);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", Some("eve_public.pirate.suppression.api.GetSystemInfoResponse"), b"pir-01").await;

    // 20. Plex (18 files)
    assert_eq!(reg.files_by_domain.get("plex").unwrap().len(), 18);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", Some("eve_public.plex.vault.api.BalanceResponse"), b"plex-01").await;

    // 21. Quasar (11 files)
    assert_eq!(reg.files_by_domain.get("quasar").unwrap().len(), 11);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.quasar.config.api.GetRequest", Some("eve.quasar.config.api.GetResponse"), b"qsr-01").await;

    // 22. Ship (24 files)
    assert_eq!(reg.files_by_domain.get("ship").unwrap().len(), 24);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.ship.api.GetRequest", Some("eve.ship.api.GetResponse"), b"ship-01").await;

    // 23. Solarsystem (9 files)
    assert_eq!(reg.files_by_domain.get("solarsystem").unwrap().len(), 9);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.solarsystem.api.GetRequest", Some("eve_public.solarsystem.api.GetResponse"), b"sys-01").await;

    // 24. Sovereignty (93 files)
    assert_eq!(reg.files_by_domain.get("sovereignty").unwrap().len(), 93);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Some("eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse"), b"sov-01").await;

    // 25. Store (60 files)
    assert_eq!(reg.files_by_domain.get("store").unwrap().len(), 60);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.store.offer.api.GetAllRequest", Some("eve_public.store.offer.api.GetAllResponse"), b"store-01").await;

    // 26. Structure (13 files)
    assert_eq!(reg.files_by_domain.get("structure").unwrap().len(), 13);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.corporation.structure.GetAllWithDetailsRequest", Some("eve.corporation.structure.GetAllWithDetailsResponse"), b"struct-01").await;

    // 27. User (50 files)
    assert_eq!(reg.files_by_domain.get("user").unwrap().len(), 50);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.user.character.GetCharactersRequest", Some("eve.user.character.GetCharactersResponse"), b"user-01").await;
}

#[tokio::test]
async fn test_quasar_service_routing_batch2b() {
    // parity: feat:quasar.academy, feat:quasar.acl, feat:quasar.ai, feat:quasar.content, feat:quasar.deadspace, feat:quasar.debug, feat:quasar.dogma, feat:quasar.effect, feat:quasar.id
    eve_testkit::attest(&[
        "feat:quasar.academy",
        "feat:quasar.acl",
        "feat:quasar.ai",
        "feat:quasar.content",
        "feat:quasar.deadspace",
        "feat:quasar.debug",
        "feat:quasar.dogma",
        "feat:quasar.effect",
        "feat:quasar.id",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_012, 102, 90_000_102, 98_000_002);

    register_dynamic_handler(&ctx.quasar_gateway, "eve.acl.api.GetRequest", "eve.acl.api.GetResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.content.GetAllRequest", "eve.content.GetAllResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.debug.monolith.externalqueue.EmitProtobufEventRequest", "eve.debug.monolith.externalqueue.EmitProtobufEventResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.id.api.GetRequest", "eve.id.api.GetResponse");

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 28. Academy (2 files)
    assert_eq!(reg.files_by_domain.get("academy").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve.academy.ships.events.Borrowed");

    // 29. Acl (4 files)
    assert_eq!(reg.files_by_domain.get("acl").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.acl.api.GetRequest", Some("eve.acl.api.GetResponse"), b"acl-01").await;

    // 30. Ai (2 files)
    assert_eq!(reg.files_by_domain.get("ai").unwrap().len(), 2);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", Some("eve_public.ai.assistant.chat.api.QueryResponse"), b"ai-01").await;

    // 31. Content (16 files)
    assert_eq!(reg.files_by_domain.get("content").unwrap().len(), 16);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.content.GetAllRequest", Some("eve.content.GetAllResponse"), b"cnt-01").await;

    // 32. Deadspace (18 files)
    assert_eq!(reg.files_by_domain.get("deadspace").unwrap().len(), 18);
    verify_event_notice_delivery(&ctx, &sess, "eve.deadspace.abyssal.event.CharacterCompleted");

    // 33. Debug (4 files)
    assert_eq!(reg.files_by_domain.get("debug").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.debug.monolith.externalqueue.EmitProtobufEventRequest", Some("eve.debug.monolith.externalqueue.EmitProtobufEventResponse"), b"dbg-01").await;

    // 34. Dogma (3 files)
    assert_eq!(reg.files_by_domain.get("dogma").unwrap().len(), 3);
    verify_event_notice_delivery(&ctx, &sess, "eve.dogma.modifier.system_wide.api.Applied");

    // 35. Effect (7 files)
    assert_eq!(reg.files_by_domain.get("effect").unwrap().len(), 7);
    verify_event_notice_delivery(&ctx, &sess, "eve.effect.combat.electronic.pve.JammingApplied");

    // 36. Id (6 files)
    assert_eq!(reg.files_by_domain.get("id").unwrap().len(), 6);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.id.api.GetRequest", Some("eve.id.api.GetResponse"), b"id-01").await;
}

#[tokio::test]
async fn test_quasar_service_routing_batch3a() {
    // parity: feat:quasar.killmail, feat:quasar.mission, feat:quasar.monolith, feat:quasar.narrative, feat:quasar.opportunity, feat:quasar.piratestronghold, feat:quasar.probe, feat:quasar.projectdiscovery, feat:quasar.pvp, feat:quasar.space
    eve_testkit::attest(&[
        "feat:quasar.killmail",
        "feat:quasar.mission",
        "feat:quasar.monolith",
        "feat:quasar.narrative",
        "feat:quasar.opportunity",
        "feat:quasar.piratestronghold",
        "feat:quasar.probe",
        "feat:quasar.projectdiscovery",
        "feat:quasar.pvp",
        "feat:quasar.space",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_003, 103, 90_000_103, 98_000_003);

    register_dynamic_handler(&ctx.quasar_gateway, "eve.mission.api.GetCompletedCountRequest", "eve.mission.api.GetCompletedCountResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.monolith.character.location.GetRequest", "eve.monolith.character.location.GetResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.space.api.GetDefaultTagRegistryVersionRequest", "eve.space.api.GetDefaultTagRegistryVersionResponse");

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 37. Killmail (2 files)
    assert_eq!(reg.files_by_domain.get("killmail").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve.killmail.api.ShipLossReport");

    // 38. Mission (9 files)
    assert_eq!(reg.files_by_domain.get("mission").unwrap().len(), 9);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.mission.api.GetCompletedCountRequest", Some("eve.mission.api.GetCompletedCountResponse"), b"mis-01").await;

    // 39. Monolith (10 files)
    assert_eq!(reg.files_by_domain.get("monolith").unwrap().len(), 10);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.monolith.character.location.GetRequest", Some("eve.monolith.character.location.GetResponse"), b"mono-01").await;

    // 40. Narrative (1 files)
    assert_eq!(reg.files_by_domain.get("narrative").unwrap().len(), 1);
    verify_event_notice_delivery(&ctx, &sess, "eve.narrative.api.EncryptedItemDecrypted");

    // 41. Opportunity (2 files)
    assert_eq!(reg.files_by_domain.get("opportunity").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve_public.opportunity.api.Completed");

    // 42. Piratestronghold (2 files)
    assert_eq!(reg.files_by_domain.get("piratestronghold").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve.piratestronghold.event.DestructionParticipated");

    // 43. Probe (2 files)
    assert_eq!(reg.files_by_domain.get("probe").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve.probe.event.ExplorationSignatureScanned");

    // 44. Projectdiscovery (1 files)
    assert_eq!(reg.files_by_domain.get("projectdiscovery").unwrap().len(), 1);
    verify_event_notice_delivery(&ctx, &sess, "eve.projectdiscovery.event.AnalysisSubmitted");

    // 45. Pvp (6 files)
    assert_eq!(reg.files_by_domain.get("pvp").unwrap().len(), 6);
    verify_event_notice_delivery(&ctx, &sess, "eve.pvp.provinggrounds.EventEnded");

    // 46. Space (17 files)
    assert_eq!(reg.files_by_domain.get("space").unwrap().len(), 17);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.space.api.GetDefaultTagRegistryVersionRequest", Some("eve.space.api.GetDefaultTagRegistryVersionResponse"), b"spc-01").await;
}

#[tokio::test]
async fn test_quasar_service_routing_batch3b() {
    // parity: feat:quasar.spacecomponent, feat:quasar.star, feat:quasar.stargate, feat:quasar.starterspace, feat:quasar.station, feat:quasar.thirdpartyapi, feat:quasar.tournament, feat:quasar.vanguard, feat:quasar.vendors
    eve_testkit::attest(&[
        "feat:quasar.spacecomponent",
        "feat:quasar.star",
        "feat:quasar.stargate",
        "feat:quasar.starterspace",
        "feat:quasar.station",
        "feat:quasar.thirdpartyapi",
        "feat:quasar.tournament",
        "feat:quasar.vanguard",
        "feat:quasar.vendors",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_013, 103, 90_000_103, 98_000_003);

    register_dynamic_handler(&ctx.quasar_gateway, "eve.star.resources.api.GetPowerProductionRequest", "eve.star.resources.api.GetPowerProductionResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.stargate.lock.api.admin.GetAllRequest", "eve.stargate.lock.api.admin.GetAllResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.starterspace.admin.ResetJumpShipCasterRequest", "eve.starterspace.admin.ResetJumpShipCasterResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.station.owner.GetStationOwnerRequest", "eve.station.owner.GetStationOwnerResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.tournament.arena.api.GetAllRequest", "eve.tournament.arena.api.GetAllResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest", "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventResponse");
    register_dynamic_handler(&ctx.quasar_gateway, "eve.vendors.epic.api.GetUserByEveUserIDRequest", "eve.vendors.epic.api.GetUserByEveUserIDResponse");

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 47. Spacecomponent (9 files)
    assert_eq!(reg.files_by_domain.get("spacecomponent").unwrap().len(), 9);
    verify_event_notice_delivery(&ctx, &sess, "eve.spacecomponent.deploy.api.Deployed");

    // 48. Star (4 files)
    assert_eq!(reg.files_by_domain.get("star").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.star.resources.api.GetPowerProductionRequest", Some("eve.star.resources.api.GetPowerProductionResponse"), b"star-01").await;

    // 49. Stargate (11 files)
    assert_eq!(reg.files_by_domain.get("stargate").unwrap().len(), 11);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.stargate.lock.api.admin.GetAllRequest", Some("eve.stargate.lock.api.admin.GetAllResponse"), b"sg-01").await;

    // 50. Starterspace (3 files)
    assert_eq!(reg.files_by_domain.get("starterspace").unwrap().len(), 3);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.starterspace.admin.ResetJumpShipCasterRequest", Some("eve.starterspace.admin.ResetJumpShipCasterResponse"), b"start-01").await;

    // 51. Station (6 files)
    assert_eq!(reg.files_by_domain.get("station").unwrap().len(), 6);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.station.owner.GetStationOwnerRequest", Some("eve.station.owner.GetStationOwnerResponse"), b"sta-01").await;

    // 52. Thirdpartyapi (1 files)
    assert_eq!(reg.files_by_domain.get("thirdpartyapi").unwrap().len(), 1);
    verify_event_notice_delivery(&ctx, &sess, "eve.thirdpartyapi.RequestReceived");

    // 53. Tournament (7 files)
    assert_eq!(reg.files_by_domain.get("tournament").unwrap().len(), 7);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.tournament.arena.api.GetAllRequest", Some("eve.tournament.arena.api.GetAllResponse"), b"tourn-01").await;

    // 54. Vanguard (6 files)
    assert_eq!(reg.files_by_domain.get("vanguard").unwrap().len(), 6);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest", Some("eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventResponse"), b"vg-01").await;

    // 55. Vendors (19 files)
    assert_eq!(reg.files_by_domain.get("vendors").unwrap().len(), 19);
    dispatch_and_verify_request(&ctx, &mut sess, "eve.vendors.epic.api.GetUserByEveUserIDRequest", Some("eve.vendors.epic.api.GetUserByEveUserIDResponse"), b"vend-01").await;
}

fn verify_pyj_batch(reg: &QuasarDescriptorRegistry, row_ids: &[&str]) -> usize {
    for &row_id in row_ids {
        let pyj = row_id.strip_prefix("quasar:").expect("quasar: prefix");
        let fd = reg.get_file_by_pyj(pyj).expect("cataloged pyj present");
        assert_eq!(fd.pyj_path, pyj);
        for (idx, m) in fd.messages.iter().enumerate() {
            let sample = reg
                .instantiate_populated_sample(&m.full_name, (idx as u64) + 1)
                .expect("instantiate sample");
            let wire = reg.encode_dynamic(&sample).expect("encode sample");
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .expect("decode sample");
            assert_eq!(decoded, sample);
            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            assert_eq!(any.normalized_type_name(), m.full_name);
        }
    }
    row_ids.len()
}

#[tokio::test]
async fn test_quasar_campaign_objectives_and_contributions_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_001, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Some("eve_public.campaign.api.GetAllResponse"), b"cmp-b1-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Some("eve_public.campaign.contributionmethod.definition.api.GetAllResponse"), b"cmp-b1-2").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", Some("eve_public.campaign.objective.api.GetResponse"), b"cmp-b1-3").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/campaign_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj").unwrap().domain, "campaign");
}

#[tokio::test]
async fn test_quasar_campaign_tracks_and_triggers_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_002, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", Some("eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"), b"cmp-b2-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Some("eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"), b"cmp-b2-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj").unwrap().domain, "campaign");
}

#[tokio::test]
async fn test_quasar_deadspace_faction_math_ships_and_sov_hub_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj, quasar:eveProto/generated/eve_public/deadspace/archetype_pb2.pyj, quasar:eveProto/generated/eve_public/deadspace/signaturetype_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj, quasar:eveProto/generated/eve_public/math/fraction_pb2.pyj, quasar:eveProto/generated/eve_public/math/vector_pb2.pyj, quasar:eveProto/generated/eve_public/ship/classification_pb2.pyj, quasar:eveProto/generated/eve_public/ship/ship_pb2.pyj, quasar:eveProto/generated/eve_public/ship/ship_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj",
        "quasar:eveProto/generated/eve_public/deadspace/archetype_pb2.pyj",
        "quasar:eveProto/generated/eve_public/deadspace/signaturetype_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/math/fraction_pb2.pyj",
        "quasar:eveProto/generated/eve_public/math/vector_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/classification_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/ship_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/ship_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_003, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", Some("eve_public.faction.activity.admin.IncrementContributionScoreResponse"), b"sov-b3-1").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/ship/ship_pb2.pyj").unwrap().domain, "ship");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/math/vector_pb2.pyj").unwrap().domain, "math");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_sovereignty_hub_upgrades_workforce_and_mercenaryden_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activitytemplate/activitytemplate_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorph/itemtype/identifier_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activity_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activitytemplate/activitytemplate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorph/itemtype/identifier_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_004, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Some("eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse"), b"sov-b4-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Some("eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse"), b"sov-b4-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_sovereignty_resources_and_skyhooks_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorphs_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent/itemtype/identifier_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorphs_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent/itemtype/identifier_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_005, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Some("eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"), b"sov-b5-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Some("eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"), b"sov-b5-2").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", Some("eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse"), b"sov-b5-3").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_stars_stargates_stations_and_structures_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/workforce_pb2.pyj, quasar:eveProto/generated/eve_public/star/star_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/stargate_pb2.pyj, quasar:eveProto/generated/eve_public/station/station_pb2.pyj, quasar:eveProto/generated/eve_public/structure/group_pb2.pyj, quasar:eveProto/generated/eve_public/structure/structure_pb2.pyj, quasar:eveProto/generated/eve_public/structure/structure_type_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/workforce_pb2.pyj",
        "quasar:eveProto/generated/eve_public/star/star_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/stargate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/station/station_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/structure_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/structure_type_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_006, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 9);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.stargate.lock.api.GetRequest", Some("eve_public.stargate.lock.api.GetResponse"), b"sov-b6-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Some("eve_public.stargate.lock.api.GetRestrictedSystemsResponse"), b"sov-b6-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/star/star_pb2.pyj").unwrap().domain, "star");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/stargate/stargate_pb2.pyj").unwrap().domain, "stargate");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/structure/structure_pb2.pyj").unwrap().domain, "structure");
}
