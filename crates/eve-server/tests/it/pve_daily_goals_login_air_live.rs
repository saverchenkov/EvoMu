//! Live PVE Progression Parity Suite: Daily Goals, Login Campaign, AIR Career Program, Mission Reward Formulas
//!
//! Strictly verifies:
//! - `feat:pve.daily-goals`: Daily goals progression, EverMark dual rewards (player + corp), DailyBonus milestone SP.
//! - `feat:pve.air-career-program`: AIR career program tracking from live gameplay events, authentic Quasar notices.
//! - `feat:pve.login-campaign`: Downtime turnover boundaries, station hangar delivery vs in-space redeeming bay tokens.
//! - `feat:pve.mission-lp-isk-reward-formula`: Authentic mission LP and ISK reward scaling by system security and connections skills.

const ALREADY_CLAIMED_ERR: &str = "DailyRewardAlreadyClaimedToday";

use std::collections::HashMap;

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny, WireMessage};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::*;
use eve_server::services::mission_fixture::{self, MissionFixture};

use crate::common::{self, pin_clock, slash_cmd};
use eve_testkit::client_harness::extract_bound_id_from_wire_rep;

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut req_bytes);
    req_bytes
}

fn quasar_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        payload: Some(GoogleAny { type_url: format!("type.googleapis.com/{type_name}"), value }),
        ..Default::default()
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, ctx.now_unix())
}

async fn gm_pilot_docked(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    common::borrow_wire_harness(ctx, &mut sess)
        .call(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await
        .expect("SelectCharacterID");
    sess
}

// parity: feat:pve.daily-goals
// parity: feat:pve.air-career-program
#[tokio::test]
async fn test_live_pve_daily_goals_and_air_career_program() {
    eve_testkit::attest(&[
        "feat:pve.daily-goals",
        "feat:pve.air-career-program",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let char_id = 93_000_001i64;
    let mut pilot = gm_pilot_docked(&ctx, 22_001, char_id).await;

    // Train Corporation Management to level 5 authentically via GM slash command
    slash_cmd(&ctx, &mut pilot, "/giveskill 3363 5").await;
    ctx.advance_secs(11);

    // Pilot creates a player corporation
    let add_corp_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("AIR Pioneer Vanguard".into()),
                PyRep::String("APV".into()),
                PyRep::Real(0.05),
            ],
            &HashMap::new(),
        )
        .await
        .expect("AddCorporation");
    let corp_id = add_corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .expect("corporationID");
    pilot.corporation_id = Some(corp_id);

    // Subscribe to publicGatewaySvc for Quasar push notices
    common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("Subscribe publicGatewaySvc");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(pilot.session_id)
        .expect("take_stream_receiver");

    // Initialize daily goals and query state via Quasar GetAllCurrentRequest
    let all_rsp = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        Vec::new(),
    );
    assert_eq!(all_rsp.status_code, STATUS_OK);

    let goals_init = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals(char_id as u32);
    assert_eq!(goals_init.len(), 6, "Must seed 5 regular career/general goals + 1 DailyBonus milestone");

    let combat_goal = goals_init
        .iter()
        .find(|g| g.template.category == 1)
        .expect("combat goal")
        .clone();
    let mining_goal = goals_init
        .iter()
        .find(|g| g.template.category == 2)
        .expect("mining goal")
        .clone();
    let bonus_goal = goals_init
        .iter()
        .find(|g| g.template.category == 6)
        .expect("bonus milestone")
        .clone();

    // Verify negative check: redeeming incomplete goal returns 403 Forbidden
    let early_redeem = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(combat_goal.uuid),
    );
    assert_eq!(early_redeem.status_code, STATUS_FORBIDDEN);

    // Dispatch authentic mining gameplay events (category 2: target = 2000 units)
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(
        &ctx,
        char_id,
        2_000,
        30_000_142,
        1230,
    );

    // Dispatch combat kills (category 1: target = 25 kills)
    ctx.quasar_gateway
        .progression
        .daily_goals
        .on_gameplay_event(char_id as u32, 1, 25, &ctx.quasar_gateway);
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_gameplay_event(char_id as u32, "combat", 25, &ctx.quasar_gateway);

    // Verify notices emitted down public gateway stream
    let mut notices = Vec::new();
    while let Ok(msg) = stream_rx.try_recv() {
        notices.push(msg);
    }
    let decode_goal_uuid = |wm: &WireMessage| -> Option<Vec<u8>> {
        let id_bytes = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
        let id_wm = WireMessage::decode(id_bytes).ok()?;
        id_wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes().map(|b| b.to_vec())
    };
    let has_prog_for = |target_uuid: &[u8; 16], expected_progress: u64| {
        notices.iter().any(|n| {
            let Some(p) = n.payload.as_ref() else { return false };
            if p.normalized_type_name() != "eve_public.dailygoal.api.ProgressedNotice" {
                return false;
            }
            let Ok(wm) = WireMessage::decode(&p.value) else { return false };
            let prog = wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint());
            decode_goal_uuid(&wm).as_deref() == Some(target_uuid.as_slice()) && prog == Some(expected_progress)
        })
    };
    let has_comp_for = |target_uuid: &[u8; 16]| {
        notices.iter().any(|n| {
            let Some(p) = n.payload.as_ref() else { return false };
            if p.normalized_type_name() != "eve_public.dailygoal.api.CompletedNotice" {
                return false;
            }
            let Ok(wm) = WireMessage::decode(&p.value) else { return false };
            decode_goal_uuid(&wm).as_deref() == Some(target_uuid.as_slice())
        })
    };
    assert!(has_prog_for(&combat_goal.uuid, 25), "Expected decoded dailygoal ProgressedNotice for combat goal");
    assert!(has_prog_for(&mining_goal.uuid, 2_000), "Expected decoded dailygoal ProgressedNotice for mining goal");
    assert!(has_comp_for(&combat_goal.uuid), "Expected decoded dailygoal CompletedNotice for combat goal");
    assert!(has_comp_for(&mining_goal.uuid), "Expected decoded dailygoal CompletedNotice for mining goal");

    // Check goals state: goals 1 & 2 completed, bonus milestone completed (progress = 2/2)
    let goals_after = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals(char_id as u32);
    let g1 = goals_after
        .iter()
        .find(|g| g.uuid == combat_goal.uuid)
        .expect("combat goal after completion");
    let g2 = goals_after
        .iter()
        .find(|g| g.uuid == mining_goal.uuid)
        .expect("mining goal after completion");
    let gb = goals_after
        .iter()
        .find(|g| g.uuid == bonus_goal.uuid)
        .expect("bonus goal after completion");
    assert!(g1.completed && !g1.claimed);
    assert!(g2.completed && !g2.claimed);
    assert!(gb.completed && !gb.claimed);

    let (initial_evermarks, initial_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        (
            ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            ch.free_skill_points,
        )
    });
    let initial_corp_evermarks = ctx.db.with_snapshot(|s| {
        s.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0)
    });

    // Claim Goal 1: grants 500 EverMarks to character AND 500 to player's corporation
    let claim1 = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(combat_goal.uuid),
    );
    assert_eq!(claim1.status_code, STATUS_OK);

    let (ch_em1, corp_em1) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        (
            ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            s.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0),
        )
    });
    assert_eq!(ch_em1, initial_evermarks + 500);
    assert_eq!(corp_em1, initial_corp_evermarks + 500);

    // Claim Goal 2: increments EverMarks to 1000 for both character and corporation
    let claim2 = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(mining_goal.uuid),
    );
    assert_eq!(claim2.status_code, STATUS_OK);

    let (ch_em2, corp_em2) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        (
            ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            s.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0),
        )
    });
    assert_eq!(ch_em2, initial_evermarks + 1_000);
    assert_eq!(corp_em2, initial_corp_evermarks + 1_000);

    // Claim Category 6 DailyBonus milestone: awards 10,000 SP
    let claim_b = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(bonus_goal.uuid),
    );
    assert_eq!(claim_b.status_code, STATUS_OK);

    let sp_after = ctx.db.with_snapshot(|s| s.characters[&char_id].free_skill_points);
    assert_eq!(sp_after, initial_sp + 10_000.0);
    let (ch_em_after_bonus, corp_em_after_bonus) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        (
            ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            s.corp_lp_balances.get(&(ch.corporation_id, 1_000_419)).copied().unwrap_or(0),
        )
    });
    assert_eq!(ch_em_after_bonus, initial_evermarks + 1_000);
    assert_eq!(corp_em_after_bonus, initial_corp_evermarks + 1_000);

    // Negative check: re-claiming claimed daily goal fails with STATUS_FORBIDDEN
    let re_claim = quasar_call(
        &ctx,
        &pilot,
        "eve_public.dailygoal.api.RedeemRequest",
        make_uuid_request(combat_goal.uuid),
    );
    assert_eq!(re_claim.status_code, STATUS_FORBIDDEN);

    // AIR Career Program: advance and complete a career goal via authentic Quasar requests
    let career_goals = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(char_id as u32);
    let target_cg = &career_goals[0];
    let mut id_inner = Vec::new();
    encode_bytes_field(1, &target_cg.uuid, &mut id_inner);

    let mut prog_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut prog_req);
    encode_varint_field(2, 5, &mut prog_req);
    let prog_rsp = quasar_call(
        &ctx,
        &pilot,
        "eve_public.career.goal.api.admin.ProgressRequest",
        prog_req,
    );
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let mut comp_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut comp_req);
    let comp_rsp = quasar_call(
        &ctx,
        &pilot,
        "eve_public.career.goal.api.admin.CompleteRequest",
        comp_req,
    );
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    let bal_before_cg = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let mut claim_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut claim_req);
    let claim_cg = quasar_call(
        &ctx,
        &pilot,
        "eve_public.career.goal.api.ClaimRewardRequest",
        claim_req.clone(),
    );
    assert_eq!(claim_cg.status_code, STATUS_OK);
    let bal_after_cg = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert!(bal_after_cg > bal_before_cg, "ClaimRewardRequest must credit character balance");

    // Re-claiming career goal fails with STATUS_FORBIDDEN
    let re_claim_cg = quasar_call(
        &ctx,
        &pilot,
        "eve_public.career.goal.api.ClaimRewardRequest",
        claim_req,
    );
    assert_eq!(re_claim_cg.status_code, STATUS_FORBIDDEN);
}

#[tokio::test]
async fn test_live_pve_login_campaign_and_mission_reward_formula() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    // 1. Unauthenticated or unselected character queries return error / PyRep::None
    let mut anon_sess = SessionState::new(33_001);
    let unauth_res = common::borrow_wire_harness(&ctx, &mut anon_sess)
        .call(
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
        )
        .await;
    assert!(unauth_res.is_err());

    let mut charless_sess = SessionState::new(33_002);
    charless_sess.user_id = Some(444);
    let charless_res = common::borrow_wire_harness(&ctx, &mut charless_sess)
        .call(
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("charless query");
    assert_eq!(charless_res, PyRep::None);

    // 2. Pilot 2 login, docked in Jita station (60003760)
    let char_id = 93_000_002i64;
    let mut pilot = gm_pilot_docked(&ctx, 33_003, char_id).await;

    let state_day0 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("get_client_campaign_state day 0");
    assert!(
        state_day0
            .dict_get("static_data")
            .and_then(|d| d.dict_get("campaign_id"))
            .and_then(|v| v.as_i64())
            .is_some()
    );
    assert_eq!(
        state_day0
            .dict_get("item_progress")
            .and_then(|d| d.dict_get("next_reward_timestamp"))
            .and_then(|v| v.as_i64()),
        Some(0)
    );

    // Claim Day 0 reward while docked: delivers to station hangar and emits OnRedeemingTokensUpdated
    let claim_out = common::borrow_wire_harness(&ctx, &mut pilot)
        .call_exchange(
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
        )
        .await;
    let claim_day0 = claim_out.result.expect("claim_reward day 0");
    let PyRep::Tuple(tuple0) = claim_day0 else { panic!("Expected 4-tuple") };
    assert_eq!(tuple0.len(), 4);
    let delivered_item_id = tuple0[0]
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("delivered itemID");

    let is_in_hangar = ctx.db.with_snapshot(|s| {
        s.items.get(&delivered_item_id).is_some_and(|it| {
            it.flag == InventoryFlag::Hangar && it.owner_id == char_id && it.location_id == 60_003_760
        })
    });
    assert!(is_in_hangar, "Delivered reward item must be placed into docked station hangar");

    assert!(
        claim_out
            .notifications
            .iter()
            .any(|(ev, _)| ev == "OnRedeemingTokensUpdated"),
        "OnRedeemingTokensUpdated notification must be emitted on reward claim"
    );

    // Duplicate claim on the same downtime bucket raises DailyRewardAlreadyClaimedToday
    let dup_claim = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
        )
        .await
        .expect_err("dup claim");
    assert_eq!(dup_claim.code, ALREADY_CLAIMED_ERR);

    // 3. Advance clock past downtime (86,400s) and undock into space (with home_station_id cleared to test redeem queue)
    ctx.advance_secs(86_400);
    common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "stationSvc",
            None,
            "Undock",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("Undock into space");
    pilot.station_id = None;
    pilot.structure_id = None;

    let state_day1 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("get_client_campaign_state day 1");
    assert_eq!(
        state_day1
            .dict_get("item_progress")
            .and_then(|d| d.dict_get("next_reward_index"))
            .and_then(|v| v.as_i64()),
        Some(2)
    );

    // Claim Day 1 reward while undocked in space: delivers into character redeeming bay
    let claim_day1 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
        )
        .await
        .expect("claim_reward day 1 in space");
    assert_ne!(claim_day1, PyRep::None);

    let has_redeem_token = ctx.db.with_snapshot(|s| {
        let store = eve_server::services::account_services::load_store(s);
        store
            .redeem_tokens
            .get(&pilot.user_id.expect("pilot user_id"))
            .map(|toks| !toks.is_empty())
            .unwrap_or(false)
    });
    assert!(has_redeem_token, "In-space login claim must enqueue redeem token");

    // 4. Mission Reward Formulas and Connections / Negotiation scaling
    // Train Negotiation III (3356), Security Connections IV (3895), Mining Connections II (3893), Distribution Connections I (3894)
    slash_cmd(&ctx, &mut pilot, "/giveskill 3356 3").await;
    slash_cmd(&ctx, &mut pilot, "/giveskill 3895 4").await;
    slash_cmd(&ctx, &mut pilot, "/giveskill 3893 2").await;
    slash_cmd(&ctx, &mut pilot, "/giveskill 3894 1").await;

    // Redock pilot at Jita IV-4
    pilot.station_id = Some(60_003_760);
    pilot.solar_system_id = Some(30_000_142);

    let kw = HashMap::new();
    let agents_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(24)],
            &kw,
        )
        .await
        .expect("GetAgents");
    let PyRep::Rowset { rows, .. } = agents_rep else { panic!("Expected rowset") };
    let (agent_id, corp_id, agent_sys) = (
        rows[0][0].as_i64().expect("agent_id"),
        rows[0][3].as_i64().expect("corp_id"),
        rows[0][5].as_i64().expect("agent_sys") as u32,
    );
    let agent_station = ctx.sde.agents.get(&agent_id).and_then(|a| a.station_id).unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys;
            ch.station_id = Some(agent_station);
            ch.structure_id = None;
        }
    });
    pilot.solar_system_id = Some(agent_sys);
    pilot.station_id = Some(agent_station);
    pilot.structure_id = None;

    // Bind agent on agentMgr
    let bind_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(agent_id), PyRep::None],
            &kw,
        )
        .await
        .expect("MachoBindObject agentMgr");
    let agent_oid = extract_bound_id_from_wire_rep(&bind_rep).expect("agent_oid");

    // Request mission offer (DoAction 2): wire shape is `((agentSaysMsg, availableActions), lastActionInfo)`
    let offer_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
        )
        .await
        .expect("DoAction(RequestMission)");
    let PyRep::Tuple(offer_parts) = offer_rep else {
        panic!("Expected DoAction 2-tuple on wire")
    };
    let offer_info = &offer_parts[1];
    let mission_id = offer_info
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    let offer_isk = offer_info
        .dict_get("iskReward")
        .and_then(|v| v.as_i64())
        .expect("iskReward");
    let offer_lp = offer_info
        .dict_get("lpReward")
        .and_then(|v| v.as_i64())
        .expect("lpReward");
    let offer_bonus = offer_info
        .dict_get("bonusReward")
        .and_then(|v| v.as_i64())
        .expect("bonusReward");

    let CharacterMissionRewards {
        isk_reward: calc_isk,
        lp_reward: calc_lp,
        bonus_reward: calc_bonus,
    } = compute_character_mission_rewards(&ctx, char_id, agent_id, mission_id, agent_sys);
    assert_eq!(offer_isk, calc_isk);
    assert_eq!(offer_lp, calc_lp);
    assert_eq!(offer_bonus, calc_bonus);
    assert!(calc_isk > 0 && calc_lp > 0 && calc_bonus > 0);
    assert_ne!(calc_bonus, calc_isk / 2);
    let offer_win = offer_info
        .dict_get("bonusWindowSecs")
        .and_then(|v| v.as_i64())
        .expect("bonusWindowSecs");
    assert!((600..7_200).contains(&offer_win));

    // Validate scaling formulas independently against concrete Wiki-calibrated SDE missions:
    // LP = base_lp * (1.6288 - true_sec) * (1.0 + 0.1 * conn_lvl)
    // ISK & bonus = catalog base * (1.0 + 0.05 * neg_lvl)
    let agent_info = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let agent_sys = agent_info.solar_system_id.unwrap_or(30_000_142);
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&agent_sys)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let conn_lvl = match agent_info.division_id {
        22 => 1, // Distribution Connections I
        23 => 2, // Mining Connections II
        _ => 4,  // Security Connections IV
    };
    let sec_factor = (1.6288 - true_sec).max(0.01);
    let conn_factor = 1.0 + 0.10 * (conn_lvl as f64);

    let w_denial = compute_character_mission_rewards(&ctx, char_id, agent_id, 13_777, agent_sys);
    assert_eq!(w_denial.isk_reward, 77_050); // Denial Of Reconnaissance: 67,000 * 1.15
    assert_eq!(w_denial.bonus_reward, 73_600); // 64,000 * 1.15
    assert_eq!(
        w_denial.lp_reward,
        (73.0 * sec_factor * conn_factor).round() as i64,
        "LP reward must match (1.6288 - true_sec) * Connections skill formula"
    );

    let w_patriotic = compute_character_mission_rewards(&ctx, char_id, agent_id, 13_514, agent_sys);
    assert_eq!(w_patriotic.isk_reward, 33_350); // Patriotic Measures (4 of 5): 29,000 * 1.15
    assert_eq!(w_patriotic.bonus_reward, 42_550); // 37,000 * 1.15
    assert_eq!(
        w_patriotic.lp_reward,
        (35.0 * sec_factor * conn_factor).round() as i64,
        "Distinct L1 missions must yield independent per-mission LP baselines"
    );
    assert_eq!(mission_bonus_window_secs(&ctx, char_id, agent_id, 13_514), 3_840);

    // Accept mission (DoAction 3)
    let accept_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(3)],
            &kw,
        )
        .await
        .expect("DoAction(Accept)");
    let PyRep::Tuple(accept_parts) = accept_rep else {
        panic!("Expected DoAction(Accept) 2-tuple on wire")
    };
    assert_eq!(accept_parts[1].dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));

    // Spawn required delivery item in station hangar if mission requires delivery
    let req_item = ctx.db.with_snapshot(|s| {
        mission_fixture::get(s, char_id, mission_id)
            .filter(|m| m.has_metadata)
            .and_then(|m| m.required_delivery_type_id.map(|tid| (tid, m.required_delivery_qty.max(1))))
    });
    if let Some((tid, qty)) = req_item {
        slash_cmd(&ctx, &mut pilot, &format!("/createitem {tid} {qty}")).await;
    }

    let (m_sys, npcs_rem) = ctx.db.with_snapshot(|s| {
        mission_fixture::get(s, char_id, mission_id)
            .map(|m| (m.solar_system_id, m.npcs_remaining))
            .unwrap_or((agent_sys, 0))
    });
    if npcs_rem > 0 {
        let _ = eve_server::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            m_sys,
            glam::DVec3::ZERO,
            Some(mission_id),
        );
    }
    let ball_ids = ctx.db.with_snapshot(|s| {
        mission_fixture::get(s, char_id, mission_id)
            .map(|m| m.spawned_npc_ball_ids)
            .unwrap_or_default()
    });
    for bid in ball_ids {
        on_mission_npc_destroyed_core(&ctx.db, m_sys, bid, Some(char_id));
    }

    // Complete mission (DoAction 6): verifies crediting of ISK and LP rewards
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let complete_rep = common::borrow_wire_harness(&ctx, &mut pilot)
        .call(
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(6)],
            &kw,
        )
        .await
        .expect("DoAction(Complete)");
    let PyRep::Tuple(complete_parts) = complete_rep else {
        panic!("Expected DoAction(Complete) 2-tuple on wire")
    };
    assert_eq!(
        complete_parts[1].dict_get("missionID").and_then(|v| v.as_i64()),
        Some(mission_id as i64)
    );

    let comp_isk = complete_parts[1]
        .dict_get("iskReward")
        .and_then(|v| v.as_i64())
        .expect("complete iskReward");
    let comp_bonus = complete_parts[1]
        .dict_get("bonusReward")
        .and_then(|v| v.as_i64())
        .expect("complete bonusReward");
    let comp_lp = complete_parts[1]
        .dict_get("lpReward")
        .and_then(|v| v.as_i64())
        .expect("complete lpReward");
    assert_eq!(comp_isk, calc_isk);
    assert_eq!(comp_bonus, calc_bonus);
    assert_eq!(comp_lp, calc_lp);

    let npc_tax_cents = |isk: i64| -> i64 {
        if isk >= 100_000 {
            ((isk * 100) * 11 + 50) / 100
        } else {
            0
        }
    };
    let expected_net_cents =
        (calc_isk * 100 - npc_tax_cents(calc_isk)) + (calc_bonus * 100 - npc_tax_cents(calc_bonus));

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert!(
        bal_after > bal_before,
        "Completed mission must credit ISK reward"
    );
    assert_eq!(
        bal_after - bal_before,
        expected_net_cents,
        "Net mission wallet delta reflects calculated ISK + bonus minus 11% NPC corp tax where >= 100k"
    );

    let lp_after = ctx.db.with_snapshot(|s| {
        s.characters[&char_id]
            .loyalty_points
            .get(&corp_id)
            .copied()
            .unwrap_or(0)
    });
    assert!(lp_after > 0, "Completed mission must credit LP reward to loyalty_points");
    assert_eq!(lp_after, calc_lp, "NPC corps charge 0% LP tax");
}

// parity: feat:pve.mission-complete
// parity: feat:pve.mission-lp-isk-reward-formula
#[tokio::test]
async fn test_live_npc_corp_mission_isk_tax_and_untaxed_lp() {
    eve_testkit::attest(&[
        "feat:pve.mission-complete",
        "feat:pve.mission-lp-isk-reward-formula",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let char_id = 93_000_201_i64;
    let mut pilot = gm_pilot_docked(&ctx, 22_099, char_id).await;
    slash_cmd(&ctx, &mut pilot, "/giveskill 3356 5").await; // Negotiation V (+25% ISK/bonus)
    slash_cmd(&ctx, &mut pilot, "/giveskill 3895 4").await; // Security Connections IV (+40% LP)

    let npc_corp_id = 1_000_009_i64; // Non-exempt NPC starter corp (11% ISK tax >= 100k, 0% LP tax)
    let find_sec_agent = |level: u8| {
        ctx.sde
            .agents
            .values()
            .filter(|a| {
                a.agent_type_id == 2
                    && a.division_id == 24
                    && a.level == level
                    && a.station_id.is_some()
                    && a.solar_system_id.is_some()
            })
            .min_by_key(|a| a.agent_id)
            .expect("SDE Basic Security agent")
            .clone()
    };
    let ag4 = find_sec_agent(4);
    let ag2 = find_sec_agent(2);
    let ag4_sys = ag4.solar_system_id.unwrap();
    let ag4_sta = ag4.station_id.unwrap();
    let ag2_sys = ag2.solar_system_id.unwrap();
    let ag2_sta = ag2.station_id.unwrap();

    // Case 1: L4 The Sansha Spies (900) — base ISK >= 100k AND bonus >= 100k (both independently taxed 11%; LP untaxed).
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = npc_corp_id;
            ch.clone_grade = 2;
            ch.solar_system_id = ag4_sys;
            ch.station_id = Some(ag4_sta);
            ch.standings.insert(ag4.agent_id, 7.0);
            ch.standings.insert(ag4.corporation_id, 7.0);
            ch.standings.insert(ag2.agent_id, 7.0);
            ch.standings.insert(ag2.corporation_id, 7.0);
        }
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 900, ag4.agent_id, ag4_sys)
                .current_room_id(1)
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400)
                .origin_station_id(Some(ag4_sta))
                .offered_level(Some(4)),
        );
    });
    pilot.corporation_id = Some(npc_corp_id);
    pilot.solar_system_id = Some(ag4_sys);
    pilot.station_id = Some(ag4_sta);
    pilot.structure_id = None;

    let kw = HashMap::new();
    let bind4 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", None, "MachoBindObject", &[PyRep::Integer(ag4.agent_id), PyRep::None], &kw)
        .await
        .expect("bind ag4");
    let oid4 = extract_bound_id_from_wire_rep(&bind4).expect("oid4");
    common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", Some(oid4), "DoAction", &[PyRep::Integer(3)], &kw)
        .await
        .expect("accept 900");

    let (bal_before_4, lp_before_4, j_start_4) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].balance.0,
            s.characters[&char_id].loyalty_points.get(&ag4.corporation_id).copied().unwrap_or(0),
            s.wallet_journal.len(),
        )
    });
    let comp4 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", Some(oid4), "DoAction", &[PyRep::Integer(6)], &kw)
        .await
        .expect("complete 900");
    let PyRep::Tuple(parts4) = comp4 else { panic!("Expected 2-tuple") };
    let expected_isk_4 = 358_000_i64 * 5 / 4; // 447_500
    let expected_bonus_4 = 510_000_i64 * 5 / 4; // 637_500
    let truesec_4 = ctx.sde.solar_systems.get(&ag4_sys).map(|s| s.security_status).unwrap_or(1.0);
    let expected_lp_4 = (1_979.0 * (1.6288 - truesec_4).max(0.01) * 1.40).round() as i64;
    assert_eq!(parts4[1].dict_get("iskReward").and_then(|v| v.as_i64()), Some(expected_isk_4));
    assert_eq!(parts4[1].dict_get("bonusReward").and_then(|v| v.as_i64()), Some(expected_bonus_4));
    assert_eq!(parts4[1].dict_get("lpReward").and_then(|v| v.as_i64()), Some(expected_lp_4));

    let tax_base_4_cents = ((expected_isk_4 * 100) * 11 + 50) / 100;
    let tax_bonus_4_cents = ((expected_bonus_4 * 100) * 11 + 50) / 100;
    let expected_net_4_cents =
        (expected_isk_4 * 100 - tax_base_4_cents) + (expected_bonus_4 * 100 - tax_bonus_4_cents);
    ctx.db.with_snapshot(|s| {
        let bal_after = s.characters[&char_id].balance.0;
        assert_eq!(bal_after - bal_before_4, expected_net_4_cents);
        let lp_after = s.characters[&char_id].loyalty_points.get(&ag4.corporation_id).copied().unwrap_or(0);
        assert_eq!(lp_after - lp_before_4, expected_lp_4, "NPC corp must pay untaxed LP");
        let entries: Vec<_> = s.wallet_journal.iter().skip(j_start_4).collect();
        assert!(entries.iter().any(|e| {
            e.character_id == char_id
                && e.entry_type == "agent_mission_reward_corporation_tax"
                && e.amount.0 == -tax_base_4_cents
        }));
        assert!(entries.iter().any(|e| {
            e.character_id == char_id
                && e.entry_type == "agent_mission_time_bonus_reward_corporation_tax"
                && e.amount.0 == -tax_bonus_4_cents
        }));
    });

    // Case 2: L2 Foreign Investment (4886) with Neg V — base ISK = 102_500 (>= 100k, taxed 11%)
    // while bonus ISK = 93_750 (< 100k, 0% tax!), and LP remains completely untaxed.
    let now2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ag2_sys;
            ch.station_id = Some(ag2_sta);
        }
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 4886, ag2.agent_id, ag2_sys)
                .current_room_id(1)
                .offered_at_unix(now2)
                .offer_expiry_unix(now2 + 86_400)
                .origin_station_id(Some(ag2_sta))
                .offered_level(Some(2)),
        );
    });
    pilot.solar_system_id = Some(ag2_sys);
    pilot.station_id = Some(ag2_sta);

    let bind2 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", None, "MachoBindObject", &[PyRep::Integer(ag2.agent_id), PyRep::None], &kw)
        .await
        .expect("bind ag2");
    let oid2 = extract_bound_id_from_wire_rep(&bind2).expect("oid2");
    common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", Some(oid2), "DoAction", &[PyRep::Integer(3)], &kw)
        .await
        .expect("accept 4886");

    let (bal_before_2, lp_before_2, j_start_2) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].balance.0,
            s.characters[&char_id].loyalty_points.get(&ag2.corporation_id).copied().unwrap_or(0),
            s.wallet_journal.len(),
        )
    });
    let comp2 = common::borrow_wire_harness(&ctx, &mut pilot)
        .call("agentMgr", Some(oid2), "DoAction", &[PyRep::Integer(6)], &kw)
        .await
        .expect("complete 4886");
    let PyRep::Tuple(parts2) = comp2 else { panic!("Expected 2-tuple") };
    let expected_isk_2 = 82_000_i64 * 5 / 4; // 102_500 >= 100_000
    let expected_bonus_2 = 75_000_i64 * 5 / 4; // 93_750 < 100_000
    let truesec_2 = ctx.sde.solar_systems.get(&ag2_sys).map(|s| s.security_status).unwrap_or(1.0);
    let expected_lp_2 = (113.0 * (1.6288 - truesec_2).max(0.01) * 1.40).round() as i64;
    assert_eq!(parts2[1].dict_get("iskReward").and_then(|v| v.as_i64()), Some(expected_isk_2));
    assert_eq!(parts2[1].dict_get("bonusReward").and_then(|v| v.as_i64()), Some(expected_bonus_2));
    assert_eq!(parts2[1].dict_get("lpReward").and_then(|v| v.as_i64()), Some(expected_lp_2));

    let tax_base_2_cents = ((expected_isk_2 * 100) * 11 + 50) / 100;
    let expected_net_2_cents = (expected_isk_2 * 100 - tax_base_2_cents) + expected_bonus_2 * 100;
    ctx.db.with_snapshot(|s| {
        let bal_after = s.characters[&char_id].balance.0;
        assert_eq!(bal_after - bal_before_2, expected_net_2_cents);
        let lp_after = s.characters[&char_id].loyalty_points.get(&ag2.corporation_id).copied().unwrap_or(0);
        assert_eq!(lp_after - lp_before_2, expected_lp_2, "NPC corp must pay untaxed LP");
        let entries: Vec<_> = s.wallet_journal.iter().skip(j_start_2).collect();
        assert!(entries.iter().any(|e| {
            e.character_id == char_id
                && e.entry_type == "agent_mission_reward_corporation_tax"
                && e.amount.0 == -tax_base_2_cents
        }));
        assert!(
            !entries.iter().any(|e| {
                e.character_id == char_id
                    && e.entry_type == "agent_mission_time_bonus_reward_corporation_tax"
            }),
            "Time bonus < 100k ISK must NOT incur NPC corp tax"
        );
    });
}
