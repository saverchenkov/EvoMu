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
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::*;

use crate::common::{self, pin_clock, slash_cmd};

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
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
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
    let add_corp_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("AIR Pioneer Vanguard".into()),
                PyRep::String("APV".into()),
                PyRep::Real(0.05),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = add_corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .expect("corporationID");
    pilot.corporation_id = Some(corp_id);

    // Subscribe to publicGatewaySvc for Quasar push notices
    ctx.dispatch_rpc(
        &mut pilot,
        "publicGatewaySvc",
        None,
        "Subscribe",
        &[],
        &HashMap::new(),
        None,
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
    assert_eq!(goals_init.len(), 5, "Must seed 4 rotating category goals + 1 DailyBonus milestone");

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
    assert!(
        notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p
            .type_url
            .ends_with("ProgressedNotice"))),
        "Expected ProgressedNotice"
    );
    assert!(
        notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p
            .type_url
            .ends_with("CompletedNotice"))),
        "Expected CompletedNotice"
    );

    // Check goals state: goals 1 & 2 completed, bonus milestone completed (progress = 2/2)
    let goals_after = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals(char_id as u32);
    let g1 = goals_after.iter().find(|g| g.uuid == combat_goal.uuid).unwrap();
    let g2 = goals_after.iter().find(|g| g.uuid == mining_goal.uuid).unwrap();
    let gb = goals_after.iter().find(|g| g.uuid == bonus_goal.uuid).unwrap();
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
    let unauth_res = ctx
        .dispatch_rpc(
            &mut anon_sess,
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(unauth_res.is_err());

    let mut charless_sess = SessionState::new(33_002);
    charless_sess.user_id = Some(444);
    let charless_res = ctx
        .dispatch_rpc(
            &mut charless_sess,
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charless query");
    assert_eq!(charless_res, PyRep::None);

    // 2. Pilot 2 login, docked in Jita station (60003760)
    let char_id = 93_000_002i64;
    let mut pilot = gm_pilot_docked(&ctx, 33_003, char_id).await;

    let state_day0 = ctx
        .dispatch_rpc(
            &mut pilot,
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
            None,
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
    let claim_day0 = ctx
        .dispatch_rpc(
            &mut pilot,
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("claim_reward day 0");
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

    let notifs_d0 = pilot.drain_notifications();
    assert!(
        notifs_d0.iter().any(|(ev, _)| ev == "OnRedeemingTokensUpdated"),
        "OnRedeemingTokensUpdated notification must be emitted on reward claim"
    );

    // Duplicate claim on the same downtime bucket raises DailyRewardAlreadyClaimedToday
    let dup_claim = ctx
        .dispatch_rpc(
            &mut pilot,
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("dup claim");
    assert_eq!(dup_claim.code, ALREADY_CLAIMED_ERR);

    // 3. Advance clock past downtime (86,400s) and undock into space (with home_station_id cleared to test redeem queue)
    ctx.advance_secs(86_400);
    ctx.dispatch_rpc(
        &mut pilot,
        "stationSvc",
        None,
        "Undock",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Undock into space");
    pilot.station_id = None;
    pilot.structure_id = None;

    let state_day1 = ctx
        .dispatch_rpc(
            &mut pilot,
            "loginCampaignManager",
            None,
            "get_client_campaign_state",
            &[],
            &HashMap::new(),
            None,
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
    let claim_day1 = ctx
        .dispatch_rpc(
            &mut pilot,
            "loginCampaignManager",
            None,
            "claim_reward",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("claim_reward day 1 in space");
    assert_ne!(claim_day1, PyRep::None);

    let has_redeem_token = ctx.db.with_snapshot(|s| {
        let store = eve_server::services::account_services::load_store(s);
        store
            .redeem_tokens
            .get(&pilot.user_id.unwrap())
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
    let agents_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("GetAgents");
    let PyRep::Rowset { rows, .. } = agents_rep else { panic!("Expected rowset") };
    let (agent_id, corp_id, agent_sys) = (
        rows[0][0].as_i64().unwrap(),
        rows[0][3].as_i64().unwrap(),
        rows[0][5].as_i64().unwrap() as u32,
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
    let bind_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(agent_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject agentMgr");
    let PyRep::Tuple(bind_tuple) = bind_rep else { panic!("Expected bind tuple") };
    let agent_oid = bind_tuple[0].as_i64().expect("agent_oid") as u64;

    // Request mission offer (DoAction 2)
    let offer_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(RequestMission)");
    let mission_id = offer_rep
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    let offer_isk = offer_rep.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap();
    let offer_lp = offer_rep.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
    let offer_bonus = offer_rep.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap();

    let CharacterMissionRewards {
        isk_reward: calc_isk,
        lp_reward: calc_lp,
        bonus_reward: calc_bonus,
    } = compute_character_mission_rewards(&ctx, char_id, agent_id, mission_id, agent_sys);
    assert_eq!(offer_isk, calc_isk);
    assert_eq!(offer_lp, calc_lp);
    assert_eq!(offer_bonus, calc_bonus);
    assert_eq!(calc_bonus, calc_isk / 2);

    // Validate scaling formulas:
    // LP = base_lp * (1.6288 - true_sec) * (1.0 + 0.1 * conn_lvl)
    // ISK = base_isk * (1.0 + 0.05 * neg_lvl)
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
    let tmpl = ctx.sde.missions.get(&mission_id).expect("mission template");
    let base_lp = tmpl.lp_omega.max(100);
    let sec_factor = (1.6288 - true_sec).max(0.01);
    let expected_lp = ((base_lp as f64) * sec_factor * (1.0 + 0.10 * (conn_lvl as f64))).round() as i64;
    let base_isk = base_lp * 1_000;
    let expected_isk = ((base_isk as f64) * (1.0 + 0.05 * 3.0)).round() as i64;

    assert_eq!(calc_lp, expected_lp, "LP reward must match Connections skill formula");
    assert_eq!(calc_isk, expected_isk, "ISK reward must match Negotiation skill formula");

    // Accept mission (DoAction 3)
    let accept_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(3)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(Accept)");
    assert_eq!(accept_rep.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));

    // Spawn required delivery item in station hangar if mission requires delivery
    let req_item = ctx.db.with_snapshot(|s| {
        load_mission_meta(s, char_id, mission_id)
            .and_then(|m| m.required_delivery_type_id.map(|tid| (tid, m.required_delivery_qty.max(1))))
    });
    if let Some((tid, qty)) = req_item {
        slash_cmd(&ctx, &mut pilot, &format!("/createitem {tid} {qty}")).await;
    }

    let ball_ids = ctx.db.with_snapshot(|s| {
        s.active_missions.get(&(char_id, mission_id)).map(|m| m.spawned_npc_ball_ids.clone()).unwrap_or_default()
    });
    for bid in ball_ids {
        on_mission_npc_destroyed_core(&ctx.db, agent_sys, bid, Some(char_id));
    }

    // Complete mission (DoAction 6): verifies crediting of ISK and LP rewards
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let complete_rep = ctx
        .dispatch_rpc(
            &mut pilot,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(6)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(Complete)");
    assert_eq!(
        complete_rep.dict_get("missionID").and_then(|v| v.as_i64()),
        Some(mission_id as i64)
    );

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert!(
        bal_after > bal_before,
        "Completed mission must credit ISK reward"
    );
    assert_eq!(
        bal_after - bal_before,
        15_985_000,
        "Net mission reward reflects calculated reward + bonus minus NPC corp tax"
    );

    let lp_after = ctx.db.with_snapshot(|s| {
        s.characters[&char_id]
            .loyalty_points
            .get(&corp_id)
            .copied()
            .unwrap_or(0)
    });
    assert!(lp_after > 0, "Completed mission must credit LP reward to loyalty_points");
}
