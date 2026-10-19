//! Comprehensive Integration Test Suite for Quasar Client Journeys (`Z-6`).
//!
//! Simulates full client launch-to-gameplay journeys over Quasar public-gateway
//! and native MachoNet transports with zero UnimplementedRPC or schema errors:
//! - `launch_login`: launch, authentication, character selection, notices subscription
//! - `daily_goals_events`: AIR daily opportunities, claim mechanics, achievements, career operations, project discovery
//! - `cosmetics_store`: ship SKIN licensing, Paragon missions, EVERMARKS balance, ship skin application
//! - `fitting_simulation`: ship Dogma attribute evaluation, module simulation, validation errors
//! - `market_orders`: wallet balance, regional market active orders, price history, wallet journal
//!
//! Tagged parity markers:
//! // parity: plan:Z-6
//! // parity: journey:launch_login
//! // parity: journey:daily_goals_events
//! // parity: journey:cosmetics_store

#![forbid(unsafe_code)]

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
    panic!("PyRep was not KeyVal with response_proto: {:?}", rep);
}

// ============================================================================
// parity: plan:Z-6 — Client Journey: launch_login
// ============================================================================

// parity: plan:Z-6
// parity: journey:launch_login
#[tokio::test]
async fn test_journey_launch_login() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // Step 1: Client launches in unauthenticated state
    let mut sess = SessionState::new(6001);

    // Unauthenticated request to character-scoped Quasar endpoint must return 401 Unauthorized
    let unauth_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-unauth".to_vec(),
        payload: Some(GetProgressionRequest { character_id: None }.to_any()),
        application_instance_uuid: b"app-jrn-0001".to_vec(),
        ..Default::default()
    };
    let unauth_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(unauth_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch unauthenticated request");
    let unauth_rsp = extract_response_proto(&unauth_rep);
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);

    // Step 2: Native MachoNet login with credentials
    let user_id = ctx.db.provision_player_account("pilot_launcher", "securepassword123");
    let login_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("pilot_launcher".into()),
                PyRep::String("securepassword123".into()),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("authentication.Login must succeed");
    assert!(login_rep.as_i64().is_some() || matches!(login_rep, PyRep::KeyVal(_)) || matches!(login_rep, PyRep::Dict(_)));
    assert_eq!(sess.user_id, Some(user_id));

    // Step 3: Quasar public gateway identity & account verification before character selection
    let ident_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-ident01".to_vec(),
        payload: Some(GetIdentityRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-jrn-0001".to_vec(),
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
        .expect("dispatch GetIdentityRequest");
    let ident_rsp = extract_response_proto(&ident_rep);
    assert_eq!(ident_rsp.status_code, STATUS_OK);
    let ident_decoded = GetIdentityResponse::from_any(&ident_rsp.payload.unwrap()).expect("decode GetIdentityResponse");
    assert_eq!(ident_decoded.user_id, user_id);
    assert!(ident_decoded.is_omega);

    let ent_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-ent001".to_vec(),
        payload: Some(GetEntitlementsRequest { user_id: Some(user_id) }.to_any()),
        application_instance_uuid: b"app-jrn-0001".to_vec(),
        ..Default::default()
    };
    let ent_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetEntitlementsRequest");
    let ent_rsp = extract_response_proto(&ent_rep);
    assert_eq!(ent_rsp.status_code, STATUS_OK);

    // Step 4: Character creation & selection via native MachoNet
    let char_record = ctx
        .db
        .create_character(user_id, "Launcher Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id;

    let chars_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetCharactersToSelect must succeed");
    assert!(matches!(chars_rep, PyRep::ObjectEx { .. }));

    let select_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(cid)],
            &empty_kw,
            None,
        )
        .await
        .expect("SelectCharacterID must succeed");
    assert!(matches!(select_rep, PyRep::KeyVal(_)));
    assert_eq!(sess.character_id, Some(cid));

    // Step 5: Quasar notice stream subscription
    let sub_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.Subscribe must succeed");
    assert!(matches!(sub_rep, PyRep::KeyVal(_)));

    // Step 6: Post-login character progression and client settings load
    let prog_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-prog01".to_vec(),
        payload: Some(GetProgressionRequest { character_id: Some(cid as u32) }.to_any()),
        application_instance_uuid: b"app-jrn-0001".to_vec(),
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
        .expect("dispatch GetProgressionRequest");
    let prog_rsp = extract_response_proto(&prog_rep);
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let prog_decoded = GetProgressionResponse::from_any(&prog_rsp.payload.unwrap()).expect("decode GetProgressionResponse");
    assert_eq!(prog_decoded.character_id, cid as u32);
    assert!(prog_decoded.total_sp > 0);

    let set_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-set001".to_vec(),
        payload: Some(GetClientSettingsRequest {
            user_id: Some(user_id),
            character_id: Some(cid as u32),
            section: None,
        }.to_any()),
        application_instance_uuid: b"app-jrn-0001".to_vec(),
        ..Default::default()
    };
    let set_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(set_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetClientSettingsRequest");
    let set_rsp = extract_response_proto(&set_rep);
    assert_eq!(set_rsp.status_code, STATUS_OK);
}

// ============================================================================
// parity: plan:Z-6 — Client Journey: daily_goals_events
// ============================================================================

// parity: plan:Z-6
// parity: journey:daily_goals_events
#[tokio::test]
async fn test_journey_daily_goals_events() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_daily", "secretpass123");
    let char_record = ctx
        .db
        .create_character(user_id, "Daily Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(6002);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // 1. Query AIR daily opportunities
    let daily_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-daily01".to_vec(),
        payload: Some(GetDailyOpportunitiesRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
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
        .expect("dispatch GetDailyOpportunitiesRequest");
    let daily_rsp = extract_response_proto(&daily_rep);
    assert_eq!(daily_rsp.status_code, STATUS_OK);
    let daily_decoded = GetDailyOpportunitiesResponse::from_any(&daily_rsp.payload.unwrap()).expect("decode GetDailyOpportunitiesResponse");
    assert_eq!(daily_decoded.character_id, cid);
    assert!(!daily_decoded.opportunities.is_empty());
    let opp_id = daily_decoded.opportunities[0].id.clone();

    // 2. Claim daily opportunity reward
    let claim_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-claim01".to_vec(),
        payload: Some(ClaimDailyOpportunityRequest {
            character_id: Some(cid),
            opportunity_id: opp_id.clone(),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
        ..Default::default()
    };
    let claim_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(claim_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch ClaimDailyOpportunityRequest");
    let claim_rsp = extract_response_proto(&claim_rep);
    assert_eq!(claim_rsp.status_code, STATUS_OK);
    let claim_decoded = ClaimDailyOpportunityResponse::from_any(&claim_rsp.payload.unwrap()).expect("decode ClaimDailyOpportunityResponse");
    assert!(claim_decoded.success);
    assert_eq!(claim_decoded.opportunity_id, opp_id);
    assert!(claim_decoded.sp_reward > 0);

    // 3. Query achievements
    let ach_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-ach0001".to_vec(),
        payload: Some(GetAchievementsRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
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
        .expect("dispatch GetAchievementsRequest");
    let ach_rsp = extract_response_proto(&ach_rep);
    assert_eq!(ach_rsp.status_code, STATUS_OK);
    let ach_decoded = GetAchievementsResponse::from_any(&ach_rsp.payload.unwrap()).expect("decode GetAchievementsResponse");
    assert_eq!(ach_decoded.character_id, cid);
    assert!(!ach_decoded.achievements.is_empty());
    let ach_id = ach_decoded.achievements[0].id.clone();

    // 4. Claim achievement reward
    let claim_ach_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-achcl01".to_vec(),
        payload: Some(ClaimAchievementRequest {
            character_id: Some(cid),
            achievement_id: ach_id.clone(),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
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
        .expect("dispatch ClaimAchievementRequest");
    let claim_ach_rsp = extract_response_proto(&claim_ach_rep);
    assert_eq!(claim_ach_rsp.status_code, STATUS_OK);
    let claim_ach_decoded = ClaimAchievementResponse::from_any(&claim_ach_rsp.payload.unwrap()).expect("decode ClaimAchievementResponse");
    assert!(claim_ach_decoded.success);
    assert_eq!(claim_ach_decoded.achievement_id, ach_id);

    // 5. Query active career operations
    let ops_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-ops0001".to_vec(),
        payload: Some(GetOperationsRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
        ..Default::default()
    };
    let ops_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ops_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetOperationsRequest");
    let ops_rsp = extract_response_proto(&ops_rep);
    assert_eq!(ops_rsp.status_code, STATUS_OK);
    let ops_decoded = GetOperationsResponse::from_any(&ops_rsp.payload.unwrap()).expect("decode GetOperationsResponse");
    assert_eq!(ops_decoded.character_id, cid);
    assert!(!ops_decoded.operations.is_empty());

    // 6. Query Project Discovery
    let pd_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-pd00001".to_vec(),
        payload: Some(GetProjectDiscoveryRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0002".to_vec(),
        ..Default::default()
    };
    let pd_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(pd_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetProjectDiscoveryRequest");
    let pd_rsp = extract_response_proto(&pd_rep);
    assert_eq!(pd_rsp.status_code, STATUS_OK);
    let pd_decoded = GetProjectDiscoveryResponse::from_any(&pd_rsp.payload.unwrap()).expect("decode GetProjectDiscoveryResponse");
    assert_eq!(pd_decoded.character_id, cid);
    assert!(pd_decoded.score_percent > 90.0);
    assert_eq!(pd_decoded.daily_xp_cap, 2000);
}

// ============================================================================
// parity: plan:Z-6 — Client Journey: cosmetics_store
// ============================================================================

// parity: plan:Z-6
// parity: journey:cosmetics_store
#[tokio::test]
async fn test_journey_cosmetics_store() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_store", "secretpass123");
    let char_record = ctx
        .db
        .create_character(user_id, "Store Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(6003);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // 1. Check initial cosmetics state (empty licenses)
    let cos_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-cos0001".to_vec(),
        payload: Some(GetCosmeticsRequest {
            character_id: Some(cid),
            ship_type_id: Some(587),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0003".to_vec(),
        ..Default::default()
    };
    let cos_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(cos_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetCosmeticsRequest");
    let cos_rsp = extract_response_proto(&cos_rep);
    assert_eq!(cos_rsp.status_code, STATUS_OK);
    let cos_decoded = GetCosmeticsResponse::from_any(&cos_rsp.payload.unwrap()).expect("decode GetCosmeticsResponse");
    assert_eq!(cos_decoded.character_id, cid);
    assert!(cos_decoded.evermarks_balance > 0);

    // 2. Query Paragon missions
    let par_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-par0001".to_vec(),
        payload: Some(GetParagonMissionsRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0003".to_vec(),
        ..Default::default()
    };
    let par_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(par_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetParagonMissionsRequest");
    let par_rsp = extract_response_proto(&par_rep);
    assert_eq!(par_rsp.status_code, STATUS_OK);
    let par_decoded = GetParagonMissionsResponse::from_any(&par_rsp.payload.unwrap()).expect("decode GetParagonMissionsResponse");
    assert_eq!(par_decoded.character_id, cid);
    assert!(par_decoded.evermarks_balance > 0);

    // 3. Grant SKIN license and apply to ship
    ctx.db.with_mut_snapshot(|snap| {
        let active_ship = snap.characters.get(&(cid as i64)).map(|c| c.active_ship_id).unwrap_or(0);
        let list = snap.licensed_skins.entry(cid as i64).or_default();
        list.push(36400);
        snap.applied_ship_skins.insert(active_ship, 36400);
    });

    // 4. Re-query cosmetics via Quasar gateway to verify synchrony
    let cos_rep2 = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(cos_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch second GetCosmeticsRequest");
    let cos_rsp2 = extract_response_proto(&cos_rep2);
    assert_eq!(cos_rsp2.status_code, STATUS_OK);
    let cos_decoded2 = GetCosmeticsResponse::from_any(&cos_rsp2.payload.unwrap()).expect("decode second GetCosmeticsResponse");
    assert_eq!(cos_decoded2.character_id, cid);
    assert!(cos_decoded2.licensed_skin_ids.contains(&36400));
    assert_eq!(cos_decoded2.applied_skin_id, Some(36400));
}

// ============================================================================
// parity: plan:Z-6 — Client Journey: fitting_simulation
// ============================================================================

// parity: plan:Z-6
#[tokio::test]
async fn test_journey_fitting_simulation() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_fit_jrn", "secretpass123");
    let char_record = ctx
        .db
        .create_character(user_id, "Fit Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(6004);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // Seed trained skills for module fitting
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(cid as i64).or_default();
        sk.insert(3300, eve_db::types::TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
        sk.insert(3318, eve_db::types::TrainedSkillRecord { type_id: 3318, level: 5, skill_points: 256_000.0 });
        sk.insert(3449, eve_db::types::TrainedSkillRecord { type_id: 3449, level: 5, skill_points: 256_000.0 });
        sk.insert(3450, eve_db::types::TrainedSkillRecord { type_id: 3450, level: 5, skill_points: 256_000.0 });
        sk.insert(3394, eve_db::types::TrainedSkillRecord { type_id: 3394, level: 5, skill_points: 256_000.0 });
    });

    // 1. Query active ship
    let ship_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-shp0001".to_vec(),
        payload: Some(GetShipRequest {
            character_id: Some(cid),
            ship_id: None,
        }.to_any()),
        application_instance_uuid: b"app-jrn-0004".to_vec(),
        ..Default::default()
    };
    let ship_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ship_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetShipRequest");
    let ship_rsp = extract_response_proto(&ship_rep);
    assert_eq!(ship_rsp.status_code, STATUS_OK);
    let ship_decoded = GetShipResponse::from_any(&ship_rsp.payload.unwrap()).expect("decode GetShipResponse");
    assert!(ship_decoded.ship_id > 0);
    assert!(ship_decoded.max_velocity > 0.0);

    // 2. Simulate baseline Rifter fit
    let sim_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-sim0001".to_vec(),
        payload: Some(SimulateFittingRequest {
            ship_type_id: 587, // Rifter
            modules: vec![
                SimulatedModule {
                    type_id: 519, // Gyrostabilizer II
                    flag_id: 11,
                    state: 2,
                    charge_type_id: None,
                },
                SimulatedModule {
                    type_id: 438, // 1MN Afterburner II
                    flag_id: 19,
                    state: 2,
                    charge_type_id: None,
                },
            ],
            character_id: Some(cid),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0004".to_vec(),
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
        .expect("dispatch SimulateFittingRequest");
    let sim_rsp = extract_response_proto(&sim_rep);
    assert_eq!(sim_rsp.status_code, STATUS_OK);
    let sim_decoded = SimulateFittingResponse::from_any(&sim_rsp.payload.unwrap()).expect("decode SimulateFittingResponse");
    assert_eq!(sim_decoded.ship_type_id, 587);
    assert!(sim_decoded.is_valid, "Errors: {:?}", sim_decoded.validation_errors);
    let stats = sim_decoded.stats.expect("stats present");
    assert!(stats.cpu_used > 0.0);
    assert!(stats.cpu_used <= stats.cpu_output);
    assert!(stats.powergrid_used > 0.0);
    assert!(stats.powergrid_used <= stats.powergrid_output);
    assert!(stats.effective_hp > 0.0);
    assert!(stats.max_velocity > 0.0);
}

// ============================================================================
// parity: plan:Z-6 — Client Journey: market_orders
// ============================================================================

// parity: plan:Z-6
#[tokio::test]
async fn test_journey_market_orders() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let user_id = ctx.db.provision_player_account("pilot_market", "secretpass123");
    let char_record = ctx
        .db
        .create_character(user_id, "Market Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create character");
    let cid = char_record.character_id as u32;

    let mut sess = SessionState::new(6005);
    sess.user_id = Some(user_id);
    sess.character_id = Some(cid as i64);

    // Seed market order in The Forge (10000002) for Tritanium (34)
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            70001,
            eve_db::types::MarketOrderRecord {
                order_id: 70001,
                owner_id: 90_000_001,
                station_id: 60003760,
                region_id: 10_000_002,
                type_id: 34, // Tritanium
                is_bid: false,
                price: eve_db::types::IskCents::from_isk(5),
                vol_entered: 100_000,
                vol_remaining: 85_000,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::types::IskCents(0),
            },
        );
        snap.market_orders.insert(
            70002,
            eve_db::types::MarketOrderRecord {
                order_id: 70002,
                owner_id: 90_000_002,
                station_id: 60003760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: eve_db::types::IskCents::from_isk(4),
                vol_entered: 50_000,
                vol_remaining: 50_000,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::types::IskCents::from_isk(200_000),
            },
        );
    });

    // 1. Query wallet balance
    let bal_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-bal0001".to_vec(),
        payload: Some(GetBalanceRequest { character_id: Some(cid) }.to_any()),
        application_instance_uuid: b"app-jrn-0005".to_vec(),
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
        .expect("dispatch GetBalanceRequest");
    let bal_rsp = extract_response_proto(&bal_rep);
    assert_eq!(bal_rsp.status_code, STATUS_OK);
    let bal_decoded = GetBalanceResponse::from_any(&bal_rsp.payload.unwrap()).expect("decode GetBalanceResponse");
    assert_eq!(bal_decoded.character_id, cid);
    assert!(bal_decoded.isk_balance_cents >= 0);

    // 2. Query market orders in The Forge for Tritanium
    let ord_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-ord0001".to_vec(),
        payload: Some(GetOrdersRequest {
            region_id: 10_000_002,
            type_id: 34,
            order_type: "all".to_string(),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0005".to_vec(),
        ..Default::default()
    };
    let ord_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ord_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetOrdersRequest");
    let ord_rsp = extract_response_proto(&ord_rep);
    assert_eq!(ord_rsp.status_code, STATUS_OK);
    let ord_decoded = GetOrdersResponse::from_any(&ord_rsp.payload.unwrap()).expect("decode GetOrdersResponse");
    assert!(!ord_decoded.orders.is_empty());
    assert!(ord_decoded.orders.iter().any(|o| o.order_id == 70001));
    assert!(ord_decoded.orders.iter().any(|o| o.order_id == 70002));

    // 3. Query market history for Tritanium
    let hist_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-hst0001".to_vec(),
        payload: Some(GetMarketHistoryRequest {
            region_id: 10_000_002,
            type_id: 34,
        }.to_any()),
        application_instance_uuid: b"app-jrn-0005".to_vec(),
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
        .expect("dispatch GetMarketHistoryRequest");
    let hist_rsp = extract_response_proto(&hist_rep);
    assert_eq!(hist_rsp.status_code, STATUS_OK);
    let hist_decoded = GetMarketHistoryResponse::from_any(&hist_rsp.payload.unwrap()).expect("decode GetMarketHistoryResponse");
    assert_eq!(hist_decoded.type_id, 34);
    assert_eq!(hist_decoded.region_id, 10_000_002);
    assert!(!hist_decoded.entries.is_empty());

    // 4. Query wallet journal
    let jrn_req = EvePublicRequest {
        correlation_uuid: b"corr-jrn-jrn0001".to_vec(),
        payload: Some(GetJournalRequest {
            character_id: Some(cid),
            limit: Some(10),
        }.to_any()),
        application_instance_uuid: b"app-jrn-0005".to_vec(),
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
        .expect("dispatch GetJournalRequest");
    let jrn_rsp = extract_response_proto(&jrn_rep);
    assert_eq!(jrn_rsp.status_code, STATUS_OK);
    let jrn_decoded = GetJournalResponse::from_any(&jrn_rsp.payload.unwrap()).expect("decode GetJournalResponse");
    assert_eq!(jrn_decoded.character_id, cid);
}
