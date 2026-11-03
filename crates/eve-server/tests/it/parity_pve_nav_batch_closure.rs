//! End-to-End Integration Tests closing 11 Navigation & PvE parity rows:
//! `feat:navigation.route-waypoints`, `feat:navigation.wormhole-statics`, `feat:navigation.warp-vector`,
//! `feat:pve.mission-complete`, `feat:pve.escalations`, `feat:pve.expeditions`,
//! `feat:pve.incursion-payout`, `feat:pve.homefront-ops`, `feat:pve.air-career-program`,
//! `feat:pve.daily-goals`, `feat:pve.login-campaign`.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
use eve_server::services::pve_special_sites::{
    ensure_special_sites_spawned, with_pve_special_reg,
};
use eve_server::services::space_destiny::{
    ensure_system_signatures_and_anomalies, wormhole_item_id_for_signature,
};
use eve_server::services::wormhole_lifecycle_ops::{
    statics_for_system_sde, system_wormhole_profile_sde, wspace_static_specs_for_system,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn ctx_with_real_sde() -> ServerContext {
    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let uname = format!("pilot_{char_id}");
    let uid = ctx.db.provision_player_account(&uname, "secret");
    common::login_pilot(ctx, &mut sess, &uname, "secret").await;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut base = snap.characters[&90_000_001].clone();
            base.character_id = char_id;
            base.name = format!("Pilot {char_id}");
            snap.characters.insert(char_id, base);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
    });
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn spawn_ship_in_space(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3, mass: f64) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, type_id),
        )
        .await;
}

async fn quasar_rpc(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("pve-nav-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"pve-nav-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(entries) = rep else { panic!("Expected KeyVal") };
    let bytes = entries
        .into_iter()
        .find(|(k, _)| k == "response_proto")
        .and_then(|(_, v)| match v { PyRep::Bytes(b) => Some(b), _ => None })
        .expect("response_proto");
    EvePublicResponse::decode_proto(&bytes).expect("decode EvePublicResponse")
}

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p.type_url);
        }
    }
    out
}

#[tokio::test]
async fn test_navigation_waypoints_warp_vector_and_wormhole_statics() {
    // parity: feat:navigation.route-waypoints, feat:navigation.warp-vector, feat:navigation.wormhole-statics
    eve_testkit::attest(&[
        "feat:navigation.route-waypoints",
        "feat:navigation.warp-vector",
        "feat:navigation.wormhole-statics",
    ]);

    let ctx = Arc::new(ctx_with_real_sde());
    let char_id = 90_000_001_i64;
    let ship_id = 840_001_i64;
    let mut sess = setup_pilot(&ctx, 10_101, char_id, JITA_SYS, Some(JITA_STATION)).await;

    // 1. Waypoints persistence across charMgr, ESI, stargate jump, dock, and ServerContext restart
    let jita_jump = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS).unwrap().clone();
    let dest_sys = jita_jump.to_solar_system_id;
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("waypoints".into()), PyRep::List(vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(JITA_STATION)])],
    )
    .await
    .expect("SaveCharacterSetting waypoints");

    let ctx2 = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::clone(&ctx.db)));
    let mut sess2 = setup_pilot(&ctx2, 10_102, char_id, JITA_SYS, Some(JITA_STATION)).await;
    let settings2 = rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap();
    assert_eq!(settings2.dict_get("waypoints"), Some(&PyRep::List(vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(JITA_STATION)])));

    ctx2.session_change_timers.lock().unwrap().remove(&char_id);
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, jita_jump.position, 1_000_000.0).await;
    let b_jita = ctx2.bind_object(&mut sess2, "beyonce", JITA_SYS as i64).unwrap();
    ctx2.dispatch_rpc(&mut sess2, "beyonce", Some(b_jita), "CmdStargateJump", &[PyRep::Integer(jita_jump.stargate_id)], &HashMap::new(), None)
        .await
        .expect("CmdStargateJump");
    assert_eq!(
        rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap().dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(JITA_STATION)]))
    );

    ctx2.session_change_timers.lock().unwrap().remove(&char_id);
    let station_pos = ctx2.sde.stations[&JITA_STATION].position;
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, station_pos, 1_000_000.0).await;
    let b_dock = ctx2.bind_object(&mut sess2, "beyonce", JITA_SYS as i64).unwrap();
    ctx2.dispatch_rpc(&mut sess2, "beyonce", Some(b_dock), "CmdDock", &[PyRep::Integer(JITA_STATION)], &HashMap::new(), None)
        .await
        .expect("CmdDock");
    assert_eq!(rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap().dict_get("waypoints"), None);

    let uid = ctx2.db.provision_player_account("wp_esi", "wp-secret");
    ctx2.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().user_id = uid; });
    let _ = ctx2.db.acquire_online_lock_with_computer_hash(uid, sess2.session_id, Some(char_id), Some("wp_hash".into()), true, ctx2.now_unix());
    let now = ctx2.now_unix();
    let code = ctx2
        .db
        .authorize_pkce("wp_esi", "wp-secret", char_id, "evomu-wp", "https://localhost/cb", vec!["esi-ui.write_waypoint.v1".into()], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now)
        .unwrap();
    let tok = ctx2.db.exchange_pkce_code(&code, "evomu-wp", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).unwrap().access_token;
    let esi = EsiRouter::new(Arc::clone(&ctx2));
    let esi_resp = esi.handle_request(&EsiHttpRequest::new("POST", "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true").with_bearer_token(&tok));
    assert_eq!(esi_resp.status_code, 204);

    // 2. Warp Vector (`warpVectorMgr.UseVector`)
    let star_pos = ctx2.sde.celestials_in_system(JITA_SYS).into_iter().find(|c| c.group_id == 6).map(|c| c.position).unwrap_or(DVec3::ZERO);
    let vec_item_id = 740_001_i64;
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.insert(vec_item_id, InventoryItemRecord { item_id: vec_item_id, type_id: 77_825, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 2, ..Default::default() });
    });
    let near_star = star_pos + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, near_star, 1_000_000.0).await;
    let vec_res = rpc(&ctx2, &mut sess2, "warpVectorMgr", "UseVector", &[PyRep::Integer(vec_item_id)]).await.expect("UseVector");
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let (_, vec_ball) = ctx2.systems.get_or_spawn(JITA_SYS).get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination, .. } = vec_ball.mode else { panic!("Expected Warp mode") };
    assert!(destination.distance(star_pos + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0)) < 10.0);

    // 3. Wormhole statics & wandering wormholes per class + static respawn on mass collapse
    let (c2_cls, _, _, _) = system_wormhole_profile_sde(&ctx2.sde, 31_000_400);
    assert_eq!(c2_cls, 2);
    assert_eq!(statics_for_system_sde(&ctx2.sde, 31_000_400).len(), 2);
    assert_eq!(wspace_static_specs_for_system(&ctx2.sde, 31_001_500).len(), 2);

    let wh_sys = 31_000_900_u32;
    ensure_system_signatures_and_anomalies(&ctx2, wh_sys).await;
    let (wh_sig_id, wh_pos, wh_type_id) = ctx2.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures.get(&wh_sys).and_then(|l| l.iter().find(|s| s.signature_id.starts_with("WST-") && !s.collapsed)).expect("C3 static");
        (sig.signature_id.clone(), DVec3::from_array(sig.position), sig.wormhole_type_id.unwrap_or(0))
    });
    assert!(wh_type_id > 0);
    let heavy_ship_id = 840_002_i64;
    for _ in 0..35 {
        let still_exists = ctx2.db.with_snapshot(|snap| snap.cosmic_signatures.get(&wh_sys).is_some_and(|l| l.iter().any(|s| s.signature_id == wh_sig_id && !s.collapsed)));
        if !still_exists { break; }
        ctx2.advance_secs(301);
        ctx2.session_change_timers.lock().unwrap().remove(&char_id);
        let _ = ctx2.db.set_character_solar_system(char_id, wh_sys, &ctx2.sde);
        spawn_ship_in_space(&ctx2, &mut sess2, char_id, heavy_ship_id, 641, wh_sys, wh_pos + DVec3::new(300.0, 0.0, 0.0), 105_000_000.0).await;
        let wh_ball_id = wormhole_item_id_for_signature(wh_sys, &wh_sig_id);
        rpc(&ctx2, &mut sess2, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)]).await.expect("WormholeJump");
    }
    let post_statics: BTreeSet<String> = ctx2.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&wh_sys).map(|l| l.iter().filter(|s| s.signature_id.starts_with("WST-") && !s.collapsed).map(|s| s.signature_id.clone()).collect()).unwrap_or_default()
    });
    assert_eq!(post_statics.len(), 1, "Collapsed static wormhole must respawn");
    assert!(!post_statics.contains(&wh_sig_id), "Respawned static must have new signature ID");
}

#[tokio::test]
async fn test_pve_mission_complete_escalations_and_expeditions() {
    // parity: feat:pve.mission-complete, feat:pve.escalations, feat:pve.expeditions
    eve_testkit::attest(&["feat:pve.mission-complete", "feat:pve.escalations", "feat:pve.expeditions"]);

    let ctx = ctx_with_real_sde();
    let char_id = 90_000_001_i64;
    let mut sess = setup_pilot(&ctx, 10_201, char_id, JITA_SYS, Some(JITA_STATION)).await;

    // 1. Career Agent mission completion -> ISK, time bonus, LP, standings, AND item reward in hangar
    let career_agent = ctx.sde.agents.values().find(|a| a.agent_type_id == 12 && a.station_id.is_some()).expect("Career agent").clone();
    let agent_station = career_agent.station_id.unwrap();
    let agent_sys = career_agent.solar_system_id.unwrap_or(JITA_SYS);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys;
            ch.station_id = Some(agent_station);
            ch.standings.insert(career_agent.corporation_id, 2.0);
        }
        snap.skills.entry(char_id).or_default().insert(3357, TrainedSkillRecord { type_id: 3357, level: 3, skill_points: 8_000.0 });
    });
    sess.solar_system_id = Some(agent_sys);
    sess.station_id = Some(agent_station);

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(career_agent.agent_id)]).await.expect("Offer");
    let mission_id = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)]).await.expect("Accept");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) { m.npcs_remaining = 0; }
        let mut delivery = None;
        for note in snap.owner_notes.values() {
            if note.item_id == mission_id as i64 {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    if let (Some(t), Some(q)) = (v.get("required_delivery_type_id").and_then(|x| x.as_u64()), v.get("required_delivery_qty").and_then(|x| x.as_i64())) {
                        delivery = Some((t as u32, q as i32));
                    }
                }
            }
        }
        if let Some((tid, qty)) = delivery {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(iid, InventoryItemRecord { item_id: iid, type_id: tid, owner_id: char_id, location_id: agent_station, flag: InventoryFlag::Hangar, quantity: qty, ..Default::default() });
        }
    });

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let lp_before = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&career_agent.corporation_id).copied().unwrap_or(0));
    let standing_before = ctx.db.with_snapshot(|s| s.characters[&char_id].standings.get(&career_agent.corporation_id).copied().unwrap_or(0.0));
    let _ = ctx.drain_notifications_for_char(char_id);

    let comp = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mission_id as i64)]).await.expect("Complete");
    assert_eq!(comp.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    assert!(comp.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let reward_item_id = comp.dict_get("rewardItemID").and_then(|v| v.as_i64()).expect("rewardItemID");

    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0) > bal_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&career_agent.corporation_id).copied().unwrap_or(0)) > lp_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].standings.get(&career_agent.corporation_id).copied().unwrap_or(0.0)) > standing_before);
    assert!(ctx.db.with_snapshot(|s| s.items.get(&reward_item_id).is_some_and(|it| it.owner_id == char_id && it.location_id == agent_station && it.flag == InventoryFlag::Hangar)));

    // 2. Escalations & Expeditions
    let ship_id = 840_005_i64;
    spawn_ship_in_space(&ctx, &mut sess, char_id, ship_id, 587, JITA_SYS, DVec3::ZERO, 1_000_000.0).await;
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    ctx.set_mechanic_rng_seed(Some(3));

    let site_id = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&JITA_SYS].iter().find(|s| s.site_category == "CombatAnomaly" && !s.active_npc_ball_ids.is_empty()).unwrap().site_id.clone()
    });
    loop {
        let balls = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&JITA_SYS].iter().find(|s| s.site_id == site_id).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default());
        if balls.is_empty() { break; }
        for bid in balls {
            ctx.set_mechanic_rng_seed(Some(3));
            destroy_specialized_npc_and_queue_bounty(&ctx, JITA_SYS, bid, char_id).await;
        }
    }

    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnEscalatingPathMessage"));
    let details = rpc(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.unwrap();
    let PyRep::Rowset { columns, rows } = details else { panic!("Expected Rowset") };
    assert_eq!(rows.len(), 1);
    let inst_id = rows[0][columns.iter().position(|c| c == "instanceID").unwrap()].as_i64().unwrap();

    for expected_step in 1..=4 {
        let cur_entry = with_pve_exploration_reg(&ctx, |reg| reg.escalations_by_char.get(&char_id).and_then(|v| v.iter().find(|e| e.instance_id == inst_id)).cloned());
        let Some(entry) = cur_entry else { break };
        assert_eq!(entry.step_index, expected_step);
        let step_sys = entry.destination_solar_system_id;
        spawn_ship_in_space(&ctx, &mut sess, char_id, ship_id, 587, step_sys, DVec3::ZERO, 1_000_000.0).await;
        let exp_site_id = format!("EXP-{inst_id}-{expected_step}");
        loop {
            let balls = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system.get(&step_sys).and_then(|l| l.iter().find(|s| s.site_id == exp_site_id)).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default());
            if balls.is_empty() { break; }
            for bid in balls { destroy_specialized_npc_and_queue_bounty(&ctx, step_sys, bid, char_id).await; }
        }
    }
    assert!(with_pve_exploration_reg(&ctx, |reg| reg.escalations_by_char.get(&char_id).cloned().unwrap_or_default()).is_empty());
}

#[tokio::test]
async fn test_pve_incursion_payout_and_homefront_ops() {
    // parity: feat:pve.incursion-payout, feat:pve.homefront-ops
    eve_testkit::attest(&["feat:pve.incursion-payout", "feat:pve.homefront-ops"]);

    let ctx = ctx_with_real_sde();
    ensure_special_sites_spawned(&ctx).await;

    let p1 = 90_000_001_i64;
    let p2 = 90_000_002_i64;
    let mut sess1 = setup_pilot(&ctx, 10_301, p1, JITA_SYS, None).await;
    let mut sess2 = setup_pilot(&ctx, 10_302, p2, JITA_SYS, None).await;

    let (inc_sys, site_id, boss_bid, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().next().expect("active incursion");
        let site = inc.sites.iter().find(|s| !s.locked_until_low_influence).expect("unlocked incursion site");
        (site.solar_system_id, site.site_id.clone(), site.boss_npc_ball_id, site.position)
    });

    spawn_ship_in_space(&ctx, &mut sess1, p1, 840_011, 587, inc_sys, site_pos, 1_000_000.0).await;
    spawn_ship_in_space(&ctx, &mut sess2, p2, 840_012, 587, inc_sys, site_pos, 1_000_000.0).await;

    let fleet_id = rpc(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p2), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    ctx.session_change_timers.lock().unwrap().remove(&p2);
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)]).await.unwrap();

    let p1_isk_before = ctx.db.with_snapshot(|s| s.characters[&p1].balance.0);
    let p2_isk_before = ctx.db.with_snapshot(|s| s.characters[&p2].balance.0);
    let p1_lp_before = ctx.db.with_snapshot(|s| s.characters[&p1].loyalty_points.get(&1_000_125).copied().unwrap_or(0));

    common::clear_incursion_site_escort_waves(&ctx, inc_sys, &site_id).await;
    let _ = ctx.systems.get_or_spawn(inc_sys).remove_ball(boss_bid).await;
    let inc_res = on_incursion_boss_destroyed(&ctx, inc_sys, boss_bid, Some(p1)).await.unwrap().unwrap();
    assert_eq!(inc_res.fleet_size, 2);
    assert!(inc_res.isk_per_pilot > 0);
    assert!(inc_res.concord_lp_per_pilot > 0);
    assert!(inc_res.influence < 1.0);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].balance.0) > p1_isk_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p2].balance.0) > p2_isk_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].loyalty_points.get(&1_000_125).copied().unwrap_or(0)) > p1_lp_before);

    let PyRep::List(log_entries) = rpc(&ctx, &mut sess1, "incursion", "GetIncursionLPLog", &[]).await.unwrap() else { panic!("Expected List") };
    assert_eq!(log_entries.len(), 1);

    // Homefront Operations
    let PyRep::Dict(hf_dict) = rpc(&ctx, &mut sess1, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[PyRep::Integer(JITA_SYS as i64)]).await.unwrap() else { panic!("Expected Dict") };
    let (_, PyRep::Dict(jita_dungeons)) = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(JITA_SYS as i64)).unwrap() else { panic!() };
    let (did_rep, PyRep::List(inst_list)) = &jita_dungeons[0] else { panic!() };
    let did = did_rep.as_i64().unwrap() as u32;
    let inst_id = inst_list[0].as_i64().unwrap();
    assert_eq!(ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id), Some(70));

    let hf_boss_bid = with_pve_special_reg(&ctx, |reg| reg.homefront_sites[&JITA_SYS].iter().find(|s| s.instance_id == inst_id).unwrap().boss_npc_ball_id);
    let hf_isk_before = ctx.db.with_snapshot(|s| s.characters[&p1].balance.0);
    let kill = ctx.systems.get_or_spawn(JITA_SYS).apply_damage_with_attacker(hf_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(p1)).await.unwrap().unwrap();
    assert!(kill.1);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].balance.0) > hf_isk_before);
}

#[tokio::test]
async fn test_pve_air_career_program_daily_goals_and_login_campaign() {
    // parity: feat:pve.air-career-program, feat:pve.daily-goals, feat:pve.login-campaign
    eve_testkit::attest(&["feat:pve.air-career-program", "feat:pve.daily-goals", "feat:pve.login-campaign"]);

    let ctx = ctx_with_real_sde();
    let char_id = 90_000_001_u32;
    let mut sess = setup_pilot(&ctx, 10_401, char_id as i64, JITA_SYS, Some(JITA_STATION)).await;
    rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    let mut notice_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).unwrap();

    // 1. AIR Career Program
    let (goal_uuid, goal_target, goal_isk, goal_sp, goal_kind) = {
        let goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(char_id);
        let g = goals.iter().find(|g| g.def.target <= 5 && (g.def.isk_reward > 0 || g.def.sp_reward > 0.0)).unwrap();
        (g.uuid, g.def.target, g.def.isk_reward, g.def.sp_reward, g.def.activity_kind.clone())
    };
    let kind_arg = if goal_kind.is_empty() { "combat" } else { goal_kind.as_str() };
    ctx.quasar_gateway.progression.career_goals.on_gameplay_event(char_id, kind_arg, goal_target, &ctx.quasar_gateway);
    assert!(drain_notices(&mut notice_rx).iter().any(|u| u.contains("career.goal.api.CompletedNotice")));

    let isk_before = ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].balance.0);
    let sp_before = ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].free_skill_points);
    let claim_cg = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", make_uuid_request(goal_uuid)).await;
    assert_eq!(claim_cg.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].balance.0) - isk_before, IskCents::from_isk(goal_isk).0);
    assert!((ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].free_skill_points) - sp_before - goal_sp).abs() < 1e-6);

    // 2. Daily Goals
    let (dg_uuid, dg_target, dg_cat) = {
        let dgs = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
        let g = dgs.iter().find(|g| g.template.category > 0).unwrap();
        (g.uuid, g.template.target, g.template.category)
    };
    ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id, dg_cat, dg_target, &ctx.quasar_gateway);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", make_uuid_request(dg_uuid)).await.status_code, STATUS_OK);
    assert_ne!(quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", make_uuid_request(dg_uuid)).await.status_code, STATUS_OK);

    ctx.advance_secs(86_400);
    assert!(ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id).iter().all(|g| !g.claimed));

    // 3. Login Campaign
    let state1 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(state1.dict_get("can_claim_today"), Some(&PyRep::Bool(true)));
    assert!(!matches!(rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::None));
    assert!(matches!(rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::None));

    ctx.advance_secs(86_400);
    let state2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(state2.dict_get("can_claim_today"), Some(&PyRep::Bool(true)));
}
