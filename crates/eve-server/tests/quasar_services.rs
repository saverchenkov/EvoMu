//! Comprehensive integration tests for Quasar Backend Services:
//! - `Q-3`: Identity & Entitlements services (`eve_public.identity.v1.*`, `eve_public.account.v1.*`)
//! - `Q-4`: Character Progression services (`eve_public.character.v1.*`)
//! - `Q-5`: Fitting Simulation services (`eve_public.fitting.v1.*`)
//! - `Q-6`: Market & Wallet services (`eve_public.market.v1.*`, `eve_public.wallet.v1.*`)

use eve_machonet::SessionState;
use eve_proto::quasar::*;
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use std::collections::HashMap;

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

// ============================================================================
// parity: plan:Q-3 — Identity & Entitlements Services
// ============================================================================

// parity: plan:Q-3
#[tokio::test]
async fn test_q3_identity_and_entitlements_services() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_alice", "securepass");
    let mut sess = SessionState::new(4001);
    sess.user_id = Some(user_id);

    // 1. Unauthenticated session returns 401 Unauthorized
    let mut unauth_sess = SessionState::new(4002);
    let ent_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-unauth01".to_vec(),
        payload: Some(GetEntitlementsRequest { user_id: None }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let unauth_rep = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch SendRequest");
    let unauth_rsp = extract_response_proto(&unauth_rep);
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);

    // 2. eve_public.identity.v1.GetEntitlementsRequest
    let auth_ent_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-ent0001".to_vec(),
        payload: Some(GetEntitlementsRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let ent_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(auth_ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetEntitlements");
    let ent_rsp = extract_response_proto(&ent_rep);
    assert_eq!(ent_rsp.status_code, STATUS_OK);
    let ent_payload = ent_rsp.payload.expect("payload present");
    let ent_decoded = GetEntitlementsResponse::from_any(&ent_payload).expect("decode GetEntitlementsResponse");
    assert_eq!(ent_decoded.user_id, user_id);
    assert!(ent_decoded.is_omega);
    assert!(ent_decoded.omega_expires_at_unix > 0);
    assert!(ent_decoded.entitlements.iter().any(|e| e == "eve.omega.clone_subscription"));
    assert!(ent_decoded.entitlements.iter().any(|e| e == "eve.market.unrestricted"));

    // 3. eve_public.identity.v1.GetIdentityRequest
    let ident_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-ident001".to_vec(),
        payload: Some(GetIdentityRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let ident_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ident_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetIdentity");
    let ident_rsp = extract_response_proto(&ident_rep);
    assert_eq!(ident_rsp.status_code, STATUS_OK);
    let ident_payload = ident_rsp.payload.expect("payload present");
    let ident_decoded = GetIdentityResponse::from_any(&ident_payload).expect("decode GetIdentityResponse");
    assert_eq!(ident_decoded.user_id, user_id);
    assert_eq!(ident_decoded.username, "pilot_alice");
    assert!(ident_decoded.is_omega);
    assert!(ident_decoded.plex_balance >= 0);

    // 4. eve_public.account.v1.GetAccountRequest
    let acct_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-acct0001".to_vec(),
        payload: Some(GetAccountRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let acct_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(acct_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetAccount");
    let acct_rsp = extract_response_proto(&acct_rep);
    assert_eq!(acct_rsp.status_code, STATUS_OK);
    let acct_payload = acct_rsp.payload.expect("payload present");
    let acct_decoded = GetAccountResponse::from_any(&acct_payload).expect("decode GetAccountResponse");
    assert_eq!(acct_decoded.user_id, user_id);
    assert_eq!(acct_decoded.username, "pilot_alice");
    assert!(acct_decoded.is_omega);
    assert!(acct_decoded.omega_expires_at_unix > 0);

    // 5. eve_public.account.v1.GetOmegaStatusRequest
    let omega_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-omega001".to_vec(),
        payload: Some(GetOmegaStatusRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let omega_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(omega_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetOmegaStatus");
    let omega_rsp = extract_response_proto(&omega_rep);
    assert_eq!(omega_rsp.status_code, STATUS_OK);
    let omega_payload = omega_rsp.payload.expect("payload present");
    let omega_decoded = GetOmegaStatusResponse::from_any(&omega_payload).expect("decode GetOmegaStatusResponse");
    assert!(omega_decoded.is_omega);
    assert_eq!(omega_decoded.status_label, "Omega Clone State");

    // 6. eve_public.account.v1.GetPlexBalanceRequest
    let plex_req = EvePublicRequest {
        correlation_uuid: b"corr-q3-plex0001".to_vec(),
        payload: Some(GetPlexBalanceRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-q3-0001".to_vec(),
        ..Default::default()
    };
    let plex_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(plex_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetPlexBalance");
    let plex_rsp = extract_response_proto(&plex_rep);
    assert_eq!(plex_rsp.status_code, STATUS_OK);
    let plex_payload = plex_rsp.payload.expect("payload present");
    let plex_decoded = GetPlexBalanceResponse::from_any(&plex_payload).expect("decode GetPlexBalanceResponse");
    assert_eq!(plex_decoded.user_id, user_id);
    assert!(plex_decoded.plex_balance >= 0);
}

// ============================================================================
// parity: plan:Q-4 — Character Progression Services
// ============================================================================

// parity: plan:Q-4
#[tokio::test]
async fn test_q4_character_progression_services() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_bob", "securepass");
    let char_record = ctx
        .db
        .create_character(user_id, "Bob Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(4003);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // 1. eve_public.character.v1.GetProgressionRequest
    let prog_req = EvePublicRequest {
        correlation_uuid: b"corr-q4-prog0001".to_vec(),
        payload: Some(GetProgressionRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-q4-0001".to_vec(),
        ..Default::default()
    };
    let prog_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(prog_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetProgression");
    let prog_rsp = extract_response_proto(&prog_rep);
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let prog_payload = prog_rsp.payload.expect("payload present");
    let prog_decoded = GetProgressionResponse::from_any(&prog_payload).expect("decode GetProgressionResponse");
    assert_eq!(prog_decoded.character_id, cid);
    assert!(prog_decoded.total_sp > 0);
    assert!(prog_decoded.attributes.is_some());
    let attrs = prog_decoded.attributes.unwrap();
    assert!(attrs.intelligence > 0.0);
    assert!(attrs.perception > 0.0);

    // 2. eve_public.character.v1.GetDailyOpportunitiesRequest
    let daily_req = EvePublicRequest {
        correlation_uuid: b"corr-q4-daily001".to_vec(),
        payload: Some(GetDailyOpportunitiesRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-q4-0001".to_vec(),
        ..Default::default()
    };
    let daily_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(daily_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetDailyOpportunities");
    let daily_rsp = extract_response_proto(&daily_rep);
    assert_eq!(daily_rsp.status_code, STATUS_OK);
    let daily_payload = daily_rsp.payload.expect("payload present");
    let daily_decoded = GetDailyOpportunitiesResponse::from_any(&daily_payload).expect("decode GetDailyOpportunitiesResponse");
    assert_eq!(daily_decoded.character_id, cid);
    assert_eq!(daily_decoded.opportunities.len(), 4);
    let first_opp = &daily_decoded.opportunities[0];
    assert_eq!(first_opp.id, "daily_enforcer_01");
    assert!(first_opp.is_completed);
    assert_eq!(first_opp.isk_reward, 500_000);
    assert_eq!(first_opp.sp_reward, 2500);

    // 3. eve_public.character.v1.ClaimDailyOpportunityRequest
    let claim_daily_req = EvePublicRequest {
        correlation_uuid: b"corr-q4-claim001".to_vec(),
        payload: Some(ClaimDailyOpportunityRequest {
            character_id: Some(cid),
            opportunity_id: "daily_enforcer_01".to_string(),
        }.to_any()),
        application_instance_uuid: b"app-q4-0001".to_vec(),
        ..Default::default()
    };
    let claim_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(claim_daily_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch ClaimDailyOpportunity");
    let claim_rsp = extract_response_proto(&claim_rep);
    assert_eq!(claim_rsp.status_code, STATUS_OK);
    let claim_payload = claim_rsp.payload.expect("payload present");
    let claim_decoded = ClaimDailyOpportunityResponse::from_any(&claim_payload).expect("decode ClaimDailyOpportunityResponse");
    assert!(claim_decoded.success);
    assert_eq!(claim_decoded.opportunity_id, "daily_enforcer_01");
    assert_eq!(claim_decoded.isk_reward, 500_000);
    assert_eq!(claim_decoded.sp_reward, 2500);

    // 4. eve_public.character.v1.GetAchievementsRequest
    let ach_req = EvePublicRequest {
        correlation_uuid: b"corr-q4-ach00001".to_vec(),
        payload: Some(GetAchievementsRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-q4-0001".to_vec(),
        ..Default::default()
    };
    let ach_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ach_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetAchievements");
    let ach_rsp = extract_response_proto(&ach_rep);
    assert_eq!(ach_rsp.status_code, STATUS_OK);
    let ach_payload = ach_rsp.payload.expect("payload present");
    let ach_decoded = GetAchievementsResponse::from_any(&ach_payload).expect("decode GetAchievementsResponse");
    assert_eq!(ach_decoded.character_id, cid);
    assert!(!ach_decoded.achievements.is_empty());
    assert!(ach_decoded.achievements.iter().any(|a| a.is_unlocked));

    // 5. eve_public.character.v1.ClaimAchievementRequest
    let claim_ach_req = EvePublicRequest {
        correlation_uuid: b"corr-q4-claimach".to_vec(),
        payload: Some(ClaimAchievementRequest {
            character_id: Some(cid),
            achievement_id: "ach_first_warp".to_string(),
        }.to_any()),
        application_instance_uuid: b"app-q4-0001".to_vec(),
        ..Default::default()
    };
    let claim_ach_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(claim_ach_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch ClaimAchievement");
    let claim_ach_rsp = extract_response_proto(&claim_ach_rep);
    assert_eq!(claim_ach_rsp.status_code, STATUS_OK);
    let claim_ach_payload = claim_ach_rsp.payload.expect("payload present");
    let claim_ach_decoded = ClaimAchievementResponse::from_any(&claim_ach_payload).expect("decode ClaimAchievementResponse");
    assert!(claim_ach_decoded.success);
    assert_eq!(claim_ach_decoded.achievement_id, "ach_first_warp");
}

// ============================================================================
// parity: plan:Q-5 — Fitting Simulation Services
// ============================================================================

// parity: plan:Q-5
#[tokio::test]
async fn test_q5_fitting_simulation_services() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_charlie", "securepass");
    let char_record = ctx
        .db
        .create_character(user_id, "Charlie Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(4004);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // Seed required skills for Charlie Pilot so fit validation succeeds
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(cid as i64).or_default();
        sk.insert(3300, eve_db::types::TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
        sk.insert(3318, eve_db::types::TrainedSkillRecord { type_id: 3318, level: 5, skill_points: 256_000.0 });
        sk.insert(3449, eve_db::types::TrainedSkillRecord { type_id: 3449, level: 5, skill_points: 256_000.0 });
        sk.insert(3450, eve_db::types::TrainedSkillRecord { type_id: 3450, level: 5, skill_points: 256_000.0 });
        sk.insert(3394, eve_db::types::TrainedSkillRecord { type_id: 3394, level: 5, skill_points: 256_000.0 });
    });

    // 1. Simulate fitting on Rifter (type ID 587) with Small Armor Repairer I (type 519, flag 11) and 1MN Afterburner I (type 438, flag 19)
    let sim_req = EvePublicRequest {
        correlation_uuid: b"corr-q5-fit00001".to_vec(),
        payload: Some(SimulateFittingRequest {
            ship_type_id: 587, // Rifter hull
            modules: vec![
                SimulatedModule {
                    type_id: 519, // Small Armor Repairer I (low power)
                    flag_id: 11,  // Low slot 0
                    state: 2,     // Active
                    charge_type_id: None,
                },
                SimulatedModule {
                    type_id: 438, // 1MN Afterburner I (medium power)
                    flag_id: 19,  // Med slot 0
                    state: 2,     // Active
                    charge_type_id: None,
                },
            ],
            character_id: Some(cid),
        }.to_any()),
        application_instance_uuid: b"app-q5-0001".to_vec(),
        ..Default::default()
    };

    let sim_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(sim_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch SimulateFitting");
    let sim_rsp = extract_response_proto(&sim_rep);
    assert_eq!(sim_rsp.status_code, STATUS_OK);
    let sim_payload = sim_rsp.payload.expect("payload present");
    let sim_decoded = SimulateFittingResponse::from_any(&sim_payload).expect("decode SimulateFittingResponse");
    assert_eq!(sim_decoded.ship_type_id, 587);
    assert!(sim_decoded.is_valid, "Fit should be valid: {:?}", sim_decoded.validation_errors);
    assert!(sim_decoded.stats.is_some());

    let stats = sim_decoded.stats.unwrap();
    assert!(stats.cpu_output > 0.0);
    assert!(stats.cpu_used > 0.0);
    assert!(stats.cpu_used <= stats.cpu_output);
    assert!(stats.powergrid_output > 0.0);
    assert!(stats.powergrid_used > 0.0);
    assert!(stats.powergrid_used <= stats.powergrid_output);
    assert!(stats.shield_hp > 0.0);
    assert!(stats.armor_hp > 0.0);
    assert!(stats.hull_hp > 0.0);
    assert!(stats.effective_hp >= stats.shield_hp + stats.armor_hp + stats.hull_hp);
    assert!(stats.max_velocity > 0.0);
    assert!(stats.mass > 0.0);

    // 2. Simulate invalid fitting: module in wrong slot (high power module in low slot flag 11)
    let invalid_sim_req = EvePublicRequest {
        correlation_uuid: b"corr-q5-invalid1".to_vec(),
        payload: Some(SimulateFittingRequest {
            ship_type_id: 587,
            modules: vec![
                SimulatedModule {
                    type_id: 483, // 200mm Autocannon I (high power) in low slot flag 11
                    flag_id: 11,
                    state: 1,
                    charge_type_id: None,
                },
            ],
            character_id: Some(cid),
        }.to_any()),
        application_instance_uuid: b"app-q5-0001".to_vec(),
        ..Default::default()
    };
    let inv_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(invalid_sim_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch invalid SimulateFitting");
    let inv_rsp = extract_response_proto(&inv_rep);
    assert_eq!(inv_rsp.status_code, STATUS_OK);
    let inv_payload = inv_rsp.payload.expect("payload present");
    let inv_decoded = SimulateFittingResponse::from_any(&inv_payload).expect("decode SimulateFittingResponse");
    assert!(!inv_decoded.is_valid);
    assert!(!inv_decoded.validation_errors.is_empty());
}

// ============================================================================
// parity: plan:Q-6 — Market & Wallet Services
// ============================================================================

// parity: plan:Q-6
#[tokio::test]
async fn test_q6_market_and_wallet_services() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_dan", "securepass");
    let char_record = ctx
        .db
        .create_character(user_id, "Dan Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(4005);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // 1. eve_public.market.v1.GetOrdersRequest (for Tritanium type 34 in region 10000002 The Forge)
    let orders_req = EvePublicRequest {
        correlation_uuid: b"corr-q6-ord00001".to_vec(),
        payload: Some(GetOrdersRequest {
            region_id: 10_000_002, // The Forge
            type_id: 34,          // Tritanium
            order_type: "all".to_string(),
        }.to_any()),
        application_instance_uuid: b"app-q6-0001".to_vec(),
        ..Default::default()
    };
    let orders_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(orders_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetOrders");
    let orders_rsp = extract_response_proto(&orders_rep);
    assert_eq!(orders_rsp.status_code, STATUS_OK);
    let orders_payload = orders_rsp.payload.expect("payload present");
    let orders_decoded = GetOrdersResponse::from_any(&orders_payload).expect("decode GetOrdersResponse");
    // Regional markets were seeded with Tritanium orders
    assert!(!orders_decoded.orders.is_empty(), "Expected seeded Tritanium orders");
    let first_order = &orders_decoded.orders[0];
    assert_eq!(first_order.type_id, 34);
    assert!(first_order.price > 0.0);
    assert!(first_order.volume_remain > 0);

    // 2. eve_public.market.v1.GetMarketHistoryRequest
    let hist_req = EvePublicRequest {
        correlation_uuid: b"corr-q6-hist0001".to_vec(),
        payload: Some(GetMarketHistoryRequest {
            region_id: 10_000_002,
            type_id: 34,
        }.to_any()),
        application_instance_uuid: b"app-q6-0001".to_vec(),
        ..Default::default()
    };
    let hist_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(hist_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetMarketHistory");
    let hist_rsp = extract_response_proto(&hist_rep);
    assert_eq!(hist_rsp.status_code, STATUS_OK);
    let hist_payload = hist_rsp.payload.expect("payload present");
    let hist_decoded = GetMarketHistoryResponse::from_any(&hist_payload).expect("decode GetMarketHistoryResponse");
    assert_eq!(hist_decoded.region_id, 10_000_002);
    assert_eq!(hist_decoded.type_id, 34);
    assert!(!hist_decoded.entries.is_empty());
    let entry = &hist_decoded.entries[0];
    assert!(entry.average_price > 0.0);
    assert!(entry.highest_price >= entry.lowest_price);

    // 3. eve_public.wallet.v1.GetBalanceRequest
    let bal_req = EvePublicRequest {
        correlation_uuid: b"corr-q6-bal00001".to_vec(),
        payload: Some(GetBalanceRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-q6-0001".to_vec(),
        ..Default::default()
    };
    let bal_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(bal_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetBalance");
    let bal_rsp = extract_response_proto(&bal_rep);
    assert_eq!(bal_rsp.status_code, STATUS_OK);
    let bal_payload = bal_rsp.payload.expect("payload present");
    let bal_decoded = GetBalanceResponse::from_any(&bal_payload).expect("decode GetBalanceResponse");
    assert_eq!(bal_decoded.character_id, cid);
    assert!(bal_decoded.isk_balance_cents >= 0);
    assert!(bal_decoded.plex_balance >= 0);

    // 4. eve_public.wallet.v1.GetJournalRequest
    let jrn_req = EvePublicRequest {
        correlation_uuid: b"corr-q6-jrn00001".to_vec(),
        payload: Some(GetJournalRequest {
            character_id: Some(cid),
            limit: Some(20),
        }.to_any()),
        application_instance_uuid: b"app-q6-0001".to_vec(),
        ..Default::default()
    };
    let jrn_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(jrn_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetJournal");
    let jrn_rsp = extract_response_proto(&jrn_rep);
    assert_eq!(jrn_rsp.status_code, STATUS_OK);
    let jrn_payload = jrn_rsp.payload.expect("payload present");
    let jrn_decoded = GetJournalResponse::from_any(&jrn_payload).expect("decode GetJournalResponse");
    assert_eq!(jrn_decoded.character_id, cid);
}
