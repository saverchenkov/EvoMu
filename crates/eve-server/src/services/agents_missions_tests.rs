//! Unit and regression tests for mission archetype classification and Paragon Heraldry / IRIS missions.

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::lp_store_catalog::PARAGON_CORP_ID;
use eve_db::{ActiveMissionRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const PARAGON_IRIS_AGENT_ID: i64 = 3_020_034;
const PARAGON_IRIS_MISSION_17013_ID: u32 = 17_013;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
            let p = std::path::PathBuf::from(env_path);
            if p.exists() {
                return Arc::new(SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"));
            }
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn bound_agent(char_id: i64, agent_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: agent_id,
        oid_str: format!("N=1:{agent_id}"),
        ref_id: 1,
    }
}

fn docked_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn move_demo_character(ctx: &ServerContext, char_id: i64, sys_id: u32, station_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&char_id)
            .expect("demo character must exist in WorldRepository::with_demo_seed()");
        ch.solar_system_id = sys_id;
        ch.station_id = station_id;
    });
}

fn distribution_career_progress(ctx: &ServerContext, char_id: i64) -> u64 {
    ctx.quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(char_id as u32)
        .iter()
        .filter(|g| g.def.activity_kind == "distribution_missions")
        .map(|g| g.progress)
        .max()
        .unwrap_or(0)
}

#[test]
fn classify_mission_archetype_excludes_heraldry_trade_dc_and_rnd_hangar_turnin_from_courier() {
    let ctx = test_ctx();
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE");
    let heraldry_tmpl = ctx
        .sde
        .missions
        .get(&PARAGON_IRIS_MISSION_17013_ID)
        .expect("Paragon Heraldry mission 17013 must exist in SDE");
    assert!(
        heraldry_tmpl.is_courier,
        "SDE mission 17013 sets is_courier=1, making runtime exclusion essential"
    );

    let heraldry_arch = classify_mission_archetype(Some(heraldry_tmpl), None, Some(iris));
    assert!(heraldry_arch.is_heraldry);
    assert!(!heraldry_arch.is_courier);
    assert!(!heraldry_arch.is_trade);
    assert!(!heraldry_arch.is_data_center);
    assert!(!heraldry_arch.is_kill);

    let trade_tmpl = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("Trade"))
        })
        .expect("SDE must contain a Trade mission with is_courier=1");
    let trade_arch = classify_mission_archetype(Some(trade_tmpl), None, None);
    assert!(trade_arch.is_trade);
    assert!(!trade_arch.is_courier);

    let dc_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5)
        .expect("SDE must contain a Data Center agent");
    let dc_arch = classify_mission_archetype(Some(trade_tmpl), None, Some(dc_agent));
    assert!(dc_arch.is_data_center);
    assert!(!dc_arch.is_trade);
    assert!(!dc_arch.is_courier);

    let rnd_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4)
        .expect("SDE must contain an R&D agent");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("BasicCourier"))
        })
        .expect("SDE must contain a BasicCourier mission");
    let now = ctx.now_unix();
    let rnd_hangar_meta = MissionTimingAndCargoMeta {
        mission_id: courier_tmpl.mission_id,
        agent_id: rnd_agent.agent_id,
        offered_at_unix: now,
        offer_expiry_unix: now + 86_400,
        accepted_at_unix: Some(now),
        accept_expiry_unix: Some(now + 604_800),
        bonus_expiry_unix: Some(now + 3_600),
        completed_at_unix: None,
        required_delivery_type_id: Some(34),
        required_delivery_qty: 20,
        granted_courier_item_id: None,
        granted_courier_qty: 0,
        is_anomic_burner: false,
        chosen_branch: None,
        destination_station_id: None,
        origin_station_id: rnd_agent.station_id,
        offered_level: Some(1),
    };
    let rnd_hangar_arch =
        classify_mission_archetype(Some(courier_tmpl), Some(&rnd_hangar_meta), Some(rnd_agent));
    assert!(!rnd_hangar_arch.is_courier);

    let mut rnd_dest_meta = rnd_hangar_meta;
    rnd_dest_meta.destination_station_id = rnd_agent.station_id;
    let rnd_dest_arch =
        classify_mission_archetype(Some(courier_tmpl), Some(&rnd_dest_meta), Some(rnd_agent));
    assert!(rnd_dest_arch.is_courier);

    let normal_arch = classify_mission_archetype(Some(courier_tmpl), None, Some(iris));
    assert!(normal_arch.is_courier);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragon_iris_heraldry_offer_accept_complete_lifecycle_succeeds_without_courier_cargo() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let st_id = iris.station_id.expect("IRIS station");
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));

    let mut sess = docked_session(char_id, sys_id, Some(st_id));
    let bound = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();

    // 1. Request mission offer via DoAction(2).
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2)],
        &kw,
    )
    .await
    .expect("DoAction(2) must be handled")
    .expect("DoAction(2) must succeed for Paragon IRIS");

    let (offered_mid, offered_meta) = ctx.db.with_snapshot(|snap| {
        let m = snap
            .active_missions
            .values()
            .find(|m| m.character_id == char_id && m.agent_id == iris.agent_id)
            .expect("DoAction(2) must create an active mission for IRIS")
            .clone();
        let meta = load_mission_meta(snap, char_id, m.mission_id)
            .expect("DoAction(2) must persist mission metadata");
        (m.mission_id, meta)
    });
    let tmpl = ctx
        .sde
        .missions
        .get(&offered_mid)
        .expect("Offered mission template must exist in SDE");
    assert!(
        tmpl.content_template
            .as_deref()
            .unwrap_or_default()
            .contains("Heraldry"),
        "IRIS must offer a Heraldry mission, got mid={offered_mid}"
    );
    assert_eq!(offered_meta.required_delivery_type_id, None);
    assert_eq!(offered_meta.destination_station_id, None);

    // 2. Journal details must not classify the Heraldry mission as Courier.
    let journal = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .expect("GetMyJournalDetails must be handled")
    .expect("GetMyJournalDetails must succeed");
    let journal_dbg = format!("{journal:?}");
    assert!(
        !journal_dbg.contains("UI/Agents/MissionTypes/Courier"),
        "Heraldry journal entry must not use Courier hint: {journal_dbg}"
    );

    let briefing = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(iris.agent_id)],
        &kw,
    )
    .await
    .expect("GetMissionBriefingInfo must be handled")
    .expect("GetMissionBriefingInfo must succeed");
    let briefing_dbg = format!("{briefing:?}");
    assert!(
        !briefing_dbg.contains("Courier") && !briefing_dbg.contains("Transport"),
        "Heraldry briefing must not use Courier objectives: {briefing_dbg}"
    );

    // 3. Accept mission via DoAction(3): must succeed without InvalidMissionLocation or courier cargo.
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .expect("DoAction(3) must be handled")
    .expect("DoAction(3) accept must succeed for Paragon Heraldry");

    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, offered_mid)];
        assert_eq!(m.state, "Accepted");
        let meta = load_mission_meta(snap, char_id, offered_mid).unwrap();
        assert_eq!(meta.granted_courier_item_id, None);
        assert!(
            !snap
                .item_custom_names
                .values()
                .any(|name| name.starts_with(&format!("courier_cargo:{char_id}:"))),
            "Heraldry accept must not grant any courier_cargo item"
        );
        assert!(
            snap.asteroids.is_empty(),
            "Heraldry accept must not spawn mining asteroids"
        );
    });

    // 4. Complete mission via DoAction(6): must credit Paragon LP and avoid distribution career goals.
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .expect("DoAction(6) must be handled")
    .expect("DoAction(6) complete must succeed for Paragon Heraldry");

    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, offered_mid)];
        assert_eq!(m.state, "Completed");
        let paragon_lp = snap.characters[&char_id]
            .loyalty_points
            .get(&PARAGON_CORP_ID)
            .copied()
            .unwrap_or(0);
        assert!(paragon_lp > 0, "Paragon IRIS mission must credit Paragon LP");
    });
    assert_eq!(
        distribution_career_progress(&ctx, char_id),
        0,
        "Completing a Paragon Heraldry mission must not increment distribution_missions career goals"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragon_iris_mission_17013_accept_and_complete_succeeds() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let m17013 = ctx
        .sde
        .missions
        .get(&PARAGON_IRIS_MISSION_17013_ID)
        .expect("SDE mission 17013 must exist");
    assert!(m17013.is_courier);

    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let st_id = iris.station_id.expect("IRIS station");
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, PARAGON_IRIS_MISSION_17013_ID),
            ActiveMissionRecord {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                character_id: char_id,
                agent_id: iris.agent_id,
                solar_system_id: sys_id,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                agent_id: iris.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(st_id),
                offered_level: Some(iris.level),
            },
        );
    });

    let mut sess = docked_session(char_id, sys_id, Some(st_id));
    let bound = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();

    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Mission 17013 accept must not return InvalidMissionLocation");

    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Mission 17013 complete must succeed");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions[&(char_id, PARAGON_IRIS_MISSION_17013_ID)].state,
            "Completed"
        );
        let paragon_lp = snap.characters[&char_id]
            .loyalty_points
            .get(&PARAGON_CORP_ID)
            .copied()
            .unwrap_or(0);
        assert!(paragon_lp > 0);
    });
    assert_eq!(distribution_career_progress(&ctx, char_id), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heraldry_colocation_and_basic_courier_cargo_contract_hold() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let iris_st_id = iris.station_id.expect("IRIS station");
    let wrong_st_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&sid| sid != iris_st_id)
        .expect("SDE must contain a second station");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, PARAGON_IRIS_MISSION_17013_ID),
            ActiveMissionRecord {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                character_id: char_id,
                agent_id: iris.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                agent_id: iris.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: Some(now),
                accept_expiry_unix: Some(now + 604_800),
                bonus_expiry_unix: Some(now + 3_600),
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(iris_st_id),
                offered_level: Some(iris.level),
            },
        );
    });

    // Completing Heraldry mission at a non-agent station must still fail with AgentNotCoLocated.
    move_demo_character(&ctx, char_id, sys_id, Some(wrong_st_id));
    let mut wrong_sess = docked_session(char_id, sys_id, Some(wrong_st_id));
    let bound_iris = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();
    let err = handle_with_bound(
        &ctx,
        &mut wrong_sess,
        Some(&bound_iris),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Completing Heraldry mission away from IRIS station must fail");
    assert_eq!(err.code, "AgentNotCoLocated");

    // True BasicCourier missions must still require required_delivery_type_id and grant cargo.
    let courier_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 2 && a.division_id == 22 && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("SDE must contain a distribution courier agent")
        .clone();
    let c_sys = courier_agent.solar_system_id.unwrap();
    let c_st = courier_agent.station_id.unwrap();
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("BasicCourier"))
        })
        .min_by_key(|m| m.mission_id)
        .expect("SDE must contain a BasicCourier mission")
        .clone();
    let c_mid = courier_tmpl.mission_id;

    move_demo_character(&ctx, char_id, c_sys, Some(c_st));
    let mut c_sess = docked_session(char_id, c_sys, Some(c_st));
    let bound_c = bound_agent(char_id, courier_agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, c_mid),
            ActiveMissionRecord {
                mission_id: c_mid,
                character_id: char_id,
                agent_id: courier_agent.agent_id,
                solar_system_id: c_sys,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: c_mid,
                agent_id: courier_agent.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: Some(c_st),
                origin_station_id: Some(c_st),
                offered_level: Some(courier_agent.level),
            },
        );
    });

    let missing_cargo_err = handle_with_bound(
        &ctx,
        &mut c_sess,
        Some(&bound_c),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("True BasicCourier mission without required_delivery_type_id must error");
    assert_eq!(missing_cargo_err.code, "InvalidMissionLocation");

    ctx.db.with_mut_snapshot(|snap| {
        let mut meta = load_mission_meta(snap, char_id, c_mid).unwrap();
        meta.required_delivery_type_id = Some(34);
        meta.required_delivery_qty = 5;
        save_mission_meta(snap, char_id, &meta);
    });
    handle_with_bound(
        &ctx,
        &mut c_sess,
        Some(&bound_c),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect("BasicCourier mission with required_delivery_type_id must accept cleanly");

    ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, c_mid).unwrap();
        let granted_id = meta
            .granted_courier_item_id
            .expect("BasicCourier accept must grant courier cargo item");
        assert_eq!(
            snap.item_custom_names.get(&granted_id).map(String::as_str),
            Some(format!("courier_cargo:{char_id}:{c_mid}").as_str())
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_rnd_turnin_without_meta_and_complete_without_courier_cargo() {
    let ctx = test_ctx();
    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("SDE must contain an R&D agent with station");
    let dc_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 5 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("SDE must contain a Data Center agent with station");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            m.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("SDE must contain a BasicCourier mission");
    let trade_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| m.content_template.as_deref().unwrap_or_default().contains("Trade"))
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("SDE must contain a Trade mission");
    let mining_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            ct.contains("Mining") && !ct.contains("Kill")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("SDE must contain a Mining mission");

    let arch_without_meta = classify_mission_archetype(Some(&courier_tmpl), None, Some(&rnd_ag));
    assert!(arch_without_meta.is_rnd_turnin);
    assert!(!arch_without_meta.is_courier);
    assert!(!arch_without_meta.is_trade);

    let arch_rnd_trade_tmpl = classify_mission_archetype(Some(&trade_tmpl), None, Some(&rnd_ag));
    assert!(arch_rnd_trade_tmpl.is_rnd_turnin);
    assert!(!arch_rnd_trade_tmpl.is_trade);

    let char_id = 90_000_001_i64;
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let rnd_st = rnd_ag.station_id.unwrap();
    move_demo_character(&ctx, char_id, rnd_sys, Some(rnd_st));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        snap.active_missions.insert(
            (char_id, courier_tmpl.mission_id),
            ActiveMissionRecord {
                mission_id: courier_tmpl.mission_id,
                character_id: char_id,
                agent_id: rnd_ag.agent_id,
                solar_system_id: rnd_sys,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        // Intentionally do NOT insert MissionTimingAndCargoMeta (meta_opt == None) to cover #1368 F-3 / REV-02.
    });

    let mut sess = docked_session(char_id, rnd_sys, Some(rnd_st));
    let mut remote_sess = docked_session(char_id, rnd_sys, None);
    let bound = bound_agent(char_id, rnd_ag.agent_id);
    let kw = HashMap::new();

    // Missing Tritanium must fail with MissionIncomplete rather than completing free.
    let err_missing = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(courier_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DoAction(6) must be handled")
    .expect_err("R&D turn-in without Tritanium must be rejected");
    assert_eq!(err_missing.code, "MissionIncomplete");

    let req_trit_qty = rnd_tritanium_required_qty(rnd_ag.level);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_701,
            eve_db::InventoryItemRecord {
                item_id: 990_701,
                type_id: 34,
                owner_id: char_id,
                location_id: rnd_st,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: req_trit_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Remote completion DoAction(7) must be rejected without consuming Tritanium.
    let err_remote = handle_with_bound(
        &ctx,
        &mut remote_sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(7),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(courier_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DoAction(7) must be handled")
    .expect_err("R&D turn-in must reject remote DoAction(7)");
    assert_eq!(err_remote.code, "CannotCompleteRemotely");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items.get(&990_701).map(|i| i.quantity),
            Some(req_trit_qty),
            "rejected DoAction(7) must not consume Tritanium"
        );
    });

    // Docked completion DoAction(6) succeeds, consumes Tritanium, and completes mission.
    let res = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(courier_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DoAction(6) must be handled")
    .expect("R&D turn-in completion with Tritanium must succeed even without meta");
    assert_eq!(res.dict_get("state"), Some(&PyRep::String("Completed".into())));
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&990_701),
            "DoAction(6) must consume exact Tritanium delivery stack"
        );
    });

    // Data Center (agent_type_id == 5) with meta_opt == None and a Mining-named template must NOT
    // fall into is_mining_mission and must require Pirate Tag (17202) x1 rather than ore.
    let dc_sys = dc_ag.solar_system_id.unwrap();
    let dc_st = dc_ag.station_id.unwrap();
    move_demo_character(&ctx, char_id, dc_sys, Some(dc_st));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mining_tmpl.mission_id),
            ActiveMissionRecord {
                mission_id: mining_tmpl.mission_id,
                character_id: char_id,
                agent_id: dc_ag.agent_id,
                solar_system_id: dc_sys,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        // Seed Veldspar only; no Pirate Tag yet.
        snap.items.insert(
            990_703,
            eve_db::InventoryItemRecord {
                item_id: 990_703,
                type_id: 1230,
                owner_id: char_id,
                location_id: dc_st,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 5_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut dc_sess = docked_session(char_id, dc_sys, Some(dc_st));
    let mut dc_remote = docked_session(char_id, dc_sys, None);
    let dc_bound = bound_agent(char_id, dc_ag.agent_id);

    let dc_missing = handle_with_bound(
        &ctx,
        &mut dc_sess,
        Some(&dc_bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(dc_ag.agent_id),
            PyRep::Integer(mining_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DC DoAction(6) handled")
    .expect_err("DC mission without Pirate Tag (17202) must fail even with ore in hangar");
    assert_eq!(dc_missing.code, "MissionIncomplete");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_702,
            eve_db::InventoryItemRecord {
                item_id: 990_702,
                type_id: 17202,
                owner_id: char_id,
                location_id: dc_st,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let dc_err_remote = handle_with_bound(
        &ctx,
        &mut dc_remote,
        Some(&dc_bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(7),
            PyRep::Integer(dc_ag.agent_id),
            PyRep::Integer(mining_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DC DoAction(7) handled")
    .expect_err("DC turn-in must reject remote DoAction(7)");
    assert_eq!(dc_err_remote.code, "CannotCompleteRemotely");

    let dc_res = handle_with_bound(
        &ctx,
        &mut dc_sess,
        Some(&dc_bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(dc_ag.agent_id),
            PyRep::Integer(mining_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DC DoAction(6) handled")
    .expect("DC turn-in with Pirate Tag must succeed without meta");
    assert_eq!(dc_res.dict_get("state"), Some(&PyRep::String("Completed".into())));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&990_702), "DC DoAction(6) must consume Pirate Tag 17202");
        assert_eq!(snap.items.get(&990_703).map(|i| i.quantity), Some(5_000));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_initial_offer_target_sys_courier_origin_sys_and_epic_arc_v24_briefing_regressions() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;

    // Clone SDE with deterministic fixtures covering #1362 (BF-1204-01, BF-1204-02) and #1368 (F-1).
    let mut sde = (*ctx.sde).clone();
    let home_sys_tmpl = sde
        .solar_systems
        .get(&30_000_142)
        .or_else(|| sde.solar_systems.values().next())
        .cloned()
        .expect("SDE must contain at least one solar system");
    let home_sys = home_sys_tmpl.solar_system_id;
    let home_sys_name = home_sys_tmpl.name.clone();

    let mut warzone_sys = home_sys_tmpl.clone();
    warzone_sys.solar_system_id = 30_002_062;
    warzone_sys.name = "Onnamon".into();
    sde.solar_systems.entry(30_002_062).or_insert(warzone_sys);
    let warzone_sys_name = sde.solar_systems[&30_002_062].name.clone();
    assert_ne!(warzone_sys_name, home_sys_name);

    let dest_sys_id = 30_002_097_u32;
    if dest_sys_id != home_sys {
        let mut dest_sys = home_sys_tmpl.clone();
        dest_sys.solar_system_id = dest_sys_id;
        dest_sys.name = "Mehatoor".into();
        sde.solar_systems.entry(dest_sys_id).or_insert(dest_sys);
    }
    let dest_sys_name = sde.solar_systems[&dest_sys_id].name.clone();
    assert_ne!(home_sys_name, dest_sys_name);

    let st_tmpl = sde
        .stations
        .values()
        .find(|s| s.solar_system_id == home_sys)
        .or_else(|| sde.stations.values().next())
        .cloned()
        .expect("SDE must contain at least one station");
    let mut origin_st = st_tmpl.clone();
    origin_st.station_id = 69_990_001;
    origin_st.solar_system_id = home_sys;
    origin_st.name = "Origin Hub".into();
    sde.stations.insert(origin_st.station_id, origin_st.clone());

    let mut dest_st = st_tmpl;
    dest_st.station_id = 69_990_002;
    dest_st.solar_system_id = dest_sys_id;
    dest_st.name = "Dest Hub".into();
    sde.stations.insert(dest_st.station_id, dest_st.clone());

    let fw_agent_id = 3_999_181_i64;
    let fw_corp_id = 1_000_181_i64;
    sde.agents.insert(
        fw_agent_id,
        eve_sde::AgentInfo {
            agent_id: fw_agent_id,
            name: "FDU Agent".into(),
            corporation_id: fw_corp_id,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 1,
            division_id: 24,
            agent_type_id: 9,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    let talk_agent_id = 3_999_182_i64;
    sde.agents.insert(
        talk_agent_id,
        eve_sde::AgentInfo {
            agent_id: talk_agent_id,
            name: "Regular Contact".into(),
            corporation_id: 1_000_009,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 2,
            division_id: 24,
            agent_type_id: 2,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    let epic_agent_id = 3_999_183_i64;
    sde.agents.insert(
        epic_agent_id,
        eve_sde::AgentInfo {
            agent_id: epic_agent_id,
            name: "Sister Alitura".into(),
            corporation_id: 1_000_130,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 1,
            division_id: 24,
            agent_type_id: 10,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    sde.epic_arcs.insert(
        29,
        eve_sde::EpicArcTemplate {
            epic_arc_id: 29,
            name: "The Blood-Stained Stars".into(),
            faction_id: Some(500_003),
            restart_interval: Some(90),
            missions_count: 50,
        },
    );

    let fw_kill_mid = 1_u32;
    let talk_mid = 999_881_u32;
    let epic_mid = 14_117_u32;
    let courier_mid = 999_882_u32;
    sde.missions.insert(
        fw_kill_mid,
        eve_sde::MissionTemplate {
            mission_id: fw_kill_mid,
            title: "FW Strike".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        talk_mid,
        eve_sde::MissionTemplate {
            mission_id: talk_mid,
            title: "Meet Contact".into(),
            is_courier: false,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_TalkToAgentMission".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        epic_mid,
        eve_sde::MissionTemplate {
            mission_id: epic_mid,
            title: "A Beacon of Hope".into(),
            is_courier: false,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_EpicArcTalkToAgent".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        courier_mid,
        eve_sde::MissionTemplate {
            mission_id: courier_mid,
            title: "Cross-System Haul".into(),
            is_courier: true,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicCourierMission".into()),
            dungeon_id: None,
        },
    );

    let fw_fac_id = sde
        .factions
        .values()
        .find(|f| f.militia_corporation_id == Some(fw_corp_id))
        .map(|f| f.faction_id as i64);
    ctx.sde = Arc::new(sde);

    // 1. BF-1204-01: Epic Arc briefing uses V24 arc name; non-Epic-Arc TalkToAgent never emits "Epic Arc".
    let epic_brief =
        build_mission_briefing_text(&ctx, epic_agent_id, epic_mid, "A Beacon of Hope", home_sys, None);
    assert!(
        epic_brief.contains("The Blood-Stained Stars") && !epic_brief.contains("Epic Arc 4"),
        "Epic Arc briefing must resolve V24 arc name: {epic_brief}"
    );
    let non_epic_talk_brief =
        build_mission_briefing_text(&ctx, talk_agent_id, talk_mid, "Meet Contact", home_sys, None);
    assert!(
        non_epic_talk_brief.starts_with("Level 2 mission liaison")
            && !non_epic_talk_brief.contains("Epic Arc"),
        "non-Epic-Arc TalkToAgent briefing must not emit Epic Arc prefix: {non_epic_talk_brief}"
    );

    // 2. BF-1204-02 (courier origin system): uses origin_station's solar system even when sys_id == dest_sys.
    let courier_meta = crate::services::agents_missions::agents_standings_arcs::MissionTimingAndCargoMeta {
        mission_id: courier_mid,
        agent_id: talk_agent_id,
        offered_at_unix: 100,
        offer_expiry_unix: 200,
        accepted_at_unix: None,
        accept_expiry_unix: None,
        bonus_expiry_unix: None,
        completed_at_unix: None,
        required_delivery_type_id: Some(3814),
        required_delivery_qty: 10,
        granted_courier_item_id: None,
        granted_courier_qty: 0,
        is_anomic_burner: false,
        chosen_branch: None,
        destination_station_id: Some(dest_st.station_id),
        origin_station_id: Some(origin_st.station_id),
        offered_level: Some(2),
    };
    let courier_brief = build_mission_briefing_text(
        &ctx,
        talk_agent_id,
        courier_mid,
        "Cross-System Haul",
        dest_sys_id,
        Some(&courier_meta),
    );
    assert!(
        courier_brief.contains(&format!("from Origin Hub ({home_sys_name}) to Dest Hub ({dest_sys_name})")),
        "Courier briefing must label origin station with origin system name: {courier_brief}"
    );

    // 3. BF-1204-02 (FW DoAction(2)): initial offer resolves encounter/objectives using warzone target_sys.
    move_demo_character(&ctx, char_id, home_sys, Some(origin_st.station_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = fw_fac_id;
            ch.standings.insert(fw_agent_id, 10.0);
        }
    });
    let mut sess = docked_session(char_id, home_sys, Some(origin_st.station_id));
    let bound = bound_agent(char_id, fw_agent_id);
    let kw = HashMap::new();
    let offer = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(fw_agent_id)],
        &kw,
    )
    .await
    .expect("FW DoAction(2) must be handled")
    .expect("FW DoAction(2) must succeed");

    let offered_mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert_eq!(offered_mid, fw_kill_mid);
    let objectives = offer.dict_get("objectives").and_then(PyRep::as_str).unwrap_or("");
    assert!(
        objectives.contains(&warzone_sys_name) && !objectives.contains(&home_sys_name),
        "FW initial offer objectives must name warzone system {warzone_sys_name} instead of home system {home_sys_name}: {objectives}"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions.get(&(char_id, offered_mid)).map(|m| m.solar_system_id),
            Some(30_002_062)
        );
    });
}

#[tokio::test]
async fn agent_warp_guards_reject_docked_cloaked_and_scrambled_without_mutating_state() {
    const CODE_SHIP_DOCKED: &str = "ShipDocked";
    const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
    const CODE_CLOAKED_CANNOT_WARP: &str = "CloakedCannotWarp";
    const CODE_WARP_SCRAMBLED: &str = "WarpScrambled";
    const CODE_SHIP_ALREADY_IN_WARP: &str = "ShipAlreadyInWarp";
    const LOC_BOOKMARK: &str = "bookmark";

    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let ship_id = 950_000_001i64;
    let sys_id = 30_000_142u32;
    let far_bm_id = 950_100_001i64;
    let near_bm_id = 950_100_002i64;
    let kw = HashMap::new();

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| {
            a.solar_system_id == Some(sys_id)
                && a.station_id
                    .and_then(|st_id| ctx.sde.stations.get(&st_id))
                    .is_some()
        })
        .expect("system must have at least one SDE station agent");
    let st_pos = ctx
        .sde
        .stations
        .get(&agent.station_id.expect("agent station verified"))
        .expect("station present in SDE")
        .position;
    let ship_origin = st_pos + glam::DVec3::new(-1_000_000.0, 0.0, 0.0);
    let far_bm_pos = ship_origin + glam::DVec3::new(1_000_000.0, 0.0, 0.0);
    let near_bm_pos = ship_origin + glam::DVec3::new(50_000.0, 0.0, 0.0);
    let bound = bound_agent(char_id, agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
        snap.bookmarks.insert(
            far_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: far_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "FarBM".into(),
                x: far_bm_pos.x,
                y: far_bm_pos.y,
                z: far_bm_pos.z,
                item_id: None,
                memo: "Far bookmark".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            near_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: near_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "NearBM".into(),
                x: near_bm_pos.x,
                y: near_bm_pos.y,
                z: near_bm_pos.z,
                item_id: None,
                memo: "On-grid bookmark".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // 1. Station-docked, structure-docked, and DB-docked sessions reject WarpToLocation / WarpToAgentInSpace / GotoLocation
    let mut sess = docked_session(char_id, sys_id, Some(60_003_760));
    sess.ship_id = Some(ship_id);
    for (method, args) in [
        ("WarpToLocation", vec![PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)]),
        ("WarpToAgentInSpace", vec![PyRep::Integer(agent.agent_id)]),
        ("GotoLocation", vec![PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)]),
    ] {
        let err_station = handle_with_bound(&ctx, &mut sess, Some(&bound), "agentMgr", method, &args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err_station.code, CODE_SHIP_DOCKED, "{method} must reject station-docked");
    }

    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001);
    let err_struct = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_struct.code, CODE_SHIP_DOCKED);
    sess.structure_id = None;

    // Session claims undocked, DB character still docked -> ShipDocked
    let err_db_docked = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_db_docked.code, CODE_SHIP_DOCKED);
    move_demo_character(&ctx, char_id, sys_id, None);

    // 2. Undocked without spawned ship ball -> ShipNotInSpace
    let err_no_ball = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_ball.code, CODE_SHIP_NOT_IN_SPACE);

    // 3. Spawn ship ball away from far BM and agent station/anchor
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: ship_origin,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    // 4. Active non-covert cloak must reject WarpToLocation, WarpToAgentInSpace, and off-grid GotoLocation
    //    WITHOUT sync_ship_cloak_and_mjfg_from_fitting clearing cs.is_cloaked!
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = false;
        cs.gate_cloak_remaining_ticks = 0;
    });
    for (method, args) in [
        ("WarpToLocation", vec![PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)]),
        ("WarpToAgentInSpace", vec![PyRep::Integer(agent.agent_id)]),
        ("GotoLocation", vec![PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)]),
    ] {
        let err_cloak = handle_with_bound(&ctx, &mut sess, Some(&bound), "agentMgr", method, &args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err_cloak.code, CODE_CLOAKED_CANNOT_WARP, "{method} must reject non-covert active cloak");
    }
    let cs_cloak = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloak.is_cloaked, "rejected cloaked warp must not decloak ship");
    assert!(ctx.active_warps.lock().unwrap().is_empty());

    // Combined active cloak + warp scramble also preserves cs.is_cloaked and initiates no warp
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 5;
    });
    let err_cloak_scram = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        err_cloak_scram.code == CODE_CLOAKED_CANNOT_WARP
            || err_cloak_scram.code == CODE_WARP_SCRAMBLED,
        "unexpected error code: {}",
        err_cloak_scram.code
    );
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);
    assert!(ctx.active_warps.lock().unwrap().is_empty());

    // 5. Uncloaked but scrambled -> WarpScrambled with zero ball/warp mutation
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.warp_scramble_strength = 5;
    });
    let err_scram = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_scram.code, CODE_WARP_SCRAMBLED);
    assert!(ctx.active_warps.lock().unwrap().is_empty());

    // 6. Subwarp GotoLocation guard when already in warp does not mutate cloak or mode
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 0;
    });
    let _ = sys_handle.set_ball_position(ship_id, ship_origin).await;
    let _ = sys_handle
        .set_movement_mode(
            ship_id,
            eve_destiny::MovementMode::new_warp(ship_origin, far_bm_pos, 3.0),
        )
        .await;
    let err_in_warp = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GotoLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(near_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_in_warp.code, CODE_SHIP_ALREADY_IN_WARP);

    // 7. Reset ball to Stop and verify valid WarpToLocation succeeds
    let _ = sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    let warp_ok = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_BOOKMARK.into()), PyRep::Integer(far_bm_id)],
        &kw,
    )
    .await
    .unwrap();
    assert!(warp_ok.is_ok(), "valid uncloaked unscrambled WarpToLocation must succeed: {warp_ok:?}");
    let (_, ball_after) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(ball_after.mode, eve_destiny::MovementMode::Warp { .. }));
}

#[tokio::test]
async fn mission_dungeon_warp_target_rejects_zarzakh_mission_and_bookmark() {
    const LOC_DUNGEON: &str = "dungeon";
    const ERR_ZARZAKH_WARP_BAN: &str = "DunBlacklistCannotWarp";

    let ctx = test_ctx();
    let zarzakh_sys = crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;
    let jita_sys = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 951_100_001_i64;
    let zz_bm_id = 951_100_010_i64;
    let kw = HashMap::new();

    // Case 1: Accepted mission & bookmark inside Zarzakh must reject with DunBlacklistCannotWarp.
    let zz_origin = glam::DVec3::new(500_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: zarzakh_sys as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = zarzakh_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.bookmarks.insert(
            zz_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: zz_bm_id,
                owner_id: char_id,
                solar_system_id: zarzakh_sys,
                label: "Zarzakh Mission Pocket".into(),
                x: 2_500_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_id, 1_901),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 1_901,
                agent_id: 3_012_050,
                solar_system_id: zarzakh_sys,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(zz_bm_id),
            },
        );
    });
    let zz_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    zz_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: zz_origin,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = docked_session(char_id, zarzakh_sys, None);
    sess.ship_id = Some(ship_id);
    let err_zz = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_DUNGEON.into()), PyRep::Integer(1_901)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_zz.code, ERR_ZARZAKH_WARP_BAN);
    let (_, zz_ball) = zz_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(zz_ball.mode, eve_destiny::MovementMode::Stop));

    // Case 2: Mission in Jita whose attached bookmark points into Zarzakh must also reject with DunBlacklistCannotWarp.
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let jita_origin = deterministic_mission_site_origin(&ctx, jita_sys, 1_902)
        + glam::DVec3::new(300_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
        }
        snap.active_missions.insert(
            (char_id, 1_902),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 1_902,
                agent_id: 3_012_050,
                solar_system_id: jita_sys,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(zz_bm_id),
            },
        );
    });
    jita_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: jita_origin,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();
    sess.solar_system_id = Some(jita_sys);
    let err_bm_zz = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(LOC_DUNGEON.into()), PyRep::Integer(1_902)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bm_zz.code, ERR_ZARZAKH_WARP_BAN);
    let (_, jita_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(jita_ball.mode, eve_destiny::MovementMode::Stop));
}

#[tokio::test]
async fn agent_mgr_dungeon_warp_rejects_unprobed_ded_and_private_escalation_without_mission() {
    const LOC_DUNGEON: &str = "dungeon";

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let other_char = 90_999_888_i64;
    let ship_id = 951_200_001_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::new(0.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    // Seed an unscanned DED combat signature, another pilot's private escalation, and a Drifter Hive.
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().extend([
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "DED-UNPROBED-1".into(),
                solar_system_id: sys_id,
                site_category: "CombatSignature".into(),
                dungeon_id: 5_101,
                dungeon_name: "Unprobed DED".into(),
                position: [20_000_000.0, 0.0, 0.0],
                probe_required: true,
                signal_strength: 0.0,
                cleared: false,
                ..Default::default()
            },
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "EXP-88901-1".into(),
                solar_system_id: sys_id,
                site_category: "Expedition".into(),
                dungeon_id: 5_102,
                dungeon_name: "Private Escalation".into(),
                position: [25_000_000.0, 0.0, 0.0],
                probe_required: false,
                signal_strength: 1.0,
                expedition_instance_id: Some(88_901),
                cleared: false,
                ..Default::default()
            },
        ]);
        reg.escalations_by_char
            .entry(other_char)
            .or_default()
            .push(crate::services::pve_exploration::EscalatingPathEntry {
                instance_id: 88_901,
                character_id: other_char,
                source_dungeon_id: 5_100,
                expedition_dungeon_id: 5_102,
                expedition_name: "Private Escalation".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 4,
                journal_text: String::new(),
                created_at_sec: ctx.now_unix(),
                expires_at_sec: ctx.now_unix() + 86_400,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 0,
            });
    });
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.insert(
            sys_id,
            crate::services::pve_special_sites::DrifterHiveRecord {
                site_id: "HIVE-JITA-TEST".into(),
                dungeon_id: 5_103,
                dungeon_name: "Sentinel".into(),
                solar_system_id: sys_id,
                element_type_id: 34_575,
                element_name: "Antikythera".into(),
                current_room: 1,
                total_rooms: 3,
                warden_npc_ball_id: 9_900_001,
                cleared: false,
                rooms: Vec::new(),
            },
        );
    });

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    for forbidden_id in [5_101_i64, 5_102_i64, 5_103_i64] {
        let err = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String(LOC_DUNGEON.into()),
                PyRep::Integer(forbidden_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            err.code, ERR_INVALID_MISSION_LOCATION,
            "agentMgr must not allow non-mission dungeon warp to {forbidden_id}"
        );
    }
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(ball.mode, eve_destiny::MovementMode::Stop));
}

#[tokio::test]
async fn v24_warp_to_location_ordinal_bookmark_referring_agent_fleet_and_p2_fixes() {
    const LOC_DUNGEON: &str = "dungeon";
    const LOC_BOOKMARK: &str = "bookmark";

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_fc = 90_000_001_i64;
    let char_wing_yes = 90_000_202_i64;
    let char_wing_no = 90_000_203_i64;
    let ship_fc = 951_300_001_i64;
    let ship_yes = 951_300_002_i64;
    let ship_no = 951_300_003_i64;
    let bm0_id = 951_300_010_i64;
    let bm1_id = 951_300_011_i64;
    let zero_st_bm_id = 951_300_012_i64;
    let mission_id = 1_910_u32;
    let kw = HashMap::new();

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| {
            a.solar_system_id == Some(sys_id)
                && a.station_id
                    .and_then(|st_id| ctx.sde.stations.get(&st_id))
                    .is_some()
        })
        .expect("system must have at least one SDE station agent");
    let st_id = agent.station_id.expect("agent station verified");
    let st_pos = ctx.sde.stations.get(&st_id).expect("SDE station").position;
    let start_pos = st_pos + glam::DVec3::new(-2_000_000.0, 0.0, 0.0);
    let bm0_pos = start_pos + glam::DVec3::new(0.0, 600_000.0, 0.0);
    let bm1_pos = start_pos + glam::DVec3::new(0.0, 0.0, 900_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_fc);
        let template_ch = snap.characters[&char_fc].clone();
        for (cid, sid) in [
            (char_fc, ship_fc),
            (char_wing_yes, ship_yes),
            (char_wing_no, ship_no),
        ] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = snap
                .characters
                .get(&cid)
                .cloned()
                .unwrap_or_else(|| template_ch.clone());
            ch.character_id = cid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.bookmarks.insert(
            bm0_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm0_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Pocket 0".into(),
                x: bm0_pos.x,
                y: bm0_pos.y,
                z: bm0_pos.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm1_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm1_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Pocket 1".into(),
                x: bm1_pos.x,
                y: bm1_pos.y,
                z: bm1_pos.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        // Zeroed station bookmark to verify SDE station coordinate fallback (Minor P2 note 2).
        snap.bookmarks.insert(
            zero_st_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: zero_st_bm_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Zeroed Station BM".into(),
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: Some(st_id),
                memo: "Station".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_fc, mission_id),
            ActiveMissionRecord {
                character_id: char_fc,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(bm0_id),
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_yes, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        (ship_no, glam::DVec3::new(2_000.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: start_pos + offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
            )
            .await
            .unwrap();
    }

    let mut sess = docked_session(char_fc, sys_id, None);
    sess.ship_id = Some(ship_fc);
    let bound = bound_agent(char_fc, agent.agent_id);

    // 1. GetMyJournalDetails emits ordinal locationNumber 0 and 1 for the two mission bookmarks.
    let journal = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected journal tuple");
    };
    let PyRep::List(m_rows) = &j_parts[0] else {
        panic!("expected mission list");
    };
    assert_eq!(m_rows.len(), 1);
    let PyRep::Tuple(m_tuple) = &m_rows[0] else {
        panic!("expected mission tuple");
    };
    let PyRep::List(bm_rows) = &m_tuple[6] else {
        panic!("expected bookmark list");
    };
    assert_eq!(bm_rows.len(), 2);
    assert_eq!(
        bm_rows[0].dict_get("locationNumber").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        bm_rows[1].dict_get("locationNumber").and_then(|v| v.as_i64()),
        Some(1)
    );

    // 2. V24 5-arg WarpToLocation("dungeon", 1, 0.0, false, referringAgentID) resolves ordinal bookmark #1!
    let res_ref = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_DUNGEON.into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_ref, PyRep::Bool(true));
    let (_, ball1) = sys_handle.get_ball_snapshot(ship_fc).await.unwrap().unwrap();
    match ball1.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(bm1_pos) < 1.0,
                "locationNumber=1 must warp to bm1 ({bm1_pos:?}), got {destination:?}"
            );
        }
        other => panic!("expected Warp to bm1, got {other:?}"),
    }

    // 3. Bound agent moniker with locationNumber=1 also resolves bookmark #1.
    let _ = sys_handle
        .set_movement_mode(ship_fc, eve_destiny::MovementMode::Stop)
        .await;
    let res_bound = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_DUNGEON.into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_bound, PyRep::Bool(true));

    // 4. Truthy args[3] (`fleet = true`) rejects NotInFleet when solo, and fleet-warps eligible members when FC.
    let _ = sys_handle
        .set_movement_mode(ship_fc, eve_destiny::MovementMode::Stop)
        .await;
    let err_no_fleet = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_DUNGEON.into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_fleet.code, "NotInFleet");

    let fleet_id = 777_901_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing_yes,
            eve_db::FleetMemberRecord {
                character_id: char_wing_yes,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing_no,
            eve_db::FleetMemberRecord {
                character_id: char_wing_no,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: false,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing_yes, char_wing_no],
                member_details,
                ..Default::default()
            },
        );
    });
    sess.fleet_id = Some(fleet_id);
    let res_fleet = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_DUNGEON.into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_fleet, PyRep::Bool(true));
    let (_, fc_ball) = sys_handle.get_ball_snapshot(ship_fc).await.unwrap().unwrap();
    let (_, yes_ball) = sys_handle
        .get_ball_snapshot(ship_yes)
        .await
        .unwrap()
        .unwrap();
    let (_, no_ball) = sys_handle
        .get_ball_snapshot(ship_no)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(fc_ball.mode, eve_destiny::MovementMode::Warp { .. }));
    assert!(matches!(yes_ball.mode, eve_destiny::MovementMode::Warp { .. }));
    assert!(matches!(no_ball.mode, eve_destiny::MovementMode::Stop));

    // 5. Zeroed station bookmark falls back to SDE station coordinates (Minor P2 note 2).
    sess.fleet_id = None;
    let _ = sys_handle
        .set_movement_mode(ship_fc, eve_destiny::MovementMode::Stop)
        .await;
    let res_st_bm = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_BOOKMARK.into()),
            PyRep::Integer(zero_st_bm_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_st_bm, PyRep::Bool(true));
    let (_, st_ball) = sys_handle.get_ball_snapshot(ship_fc).await.unwrap().unwrap();
    match st_ball.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(st_pos) < 100_000.0,
                "zeroed station bookmark must fall back to SDE station coords ({st_pos:?}), got {destination:?}"
            );
        }
        other => panic!("expected Warp to station coords, got {other:?}"),
    }

    // 6. Capacitor-shortened warp staying inside the same dungeon pocket passes adjusted dest_pos
    //    to `on_warp_start_leave_dungeon_if_needed` and does NOT emit `OnExitingDungeon` (Minor P2 note 1).
    let entry_pos = glam::DVec3::new(840_000.0, 0.0, 0.0);
    let room1_pos = glam::DVec3::new(1_000_000.0, 0.0, 0.0);
    let far_out_bm_id = 951_300_013_i64;
    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            958_001,
            eve_db::DungeonRuntimeInstance {
                instance_id: 958_001,
                dungeon_id: 9_801,
                dungeon_name: "CapClamp Pocket".into(),
                category: "Combat".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: char_fc,
                base_position: room1_pos.to_array(),
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: Some(entry_pos.to_array()),
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![eve_db::DungeonRuntimeRoom {
                    room_index: 1,
                    room_id: 9_801_01,
                    room_name: "Room 1".into(),
                    position: room1_pos.to_array(),
                    gate_destination_pos: room1_pos.to_array(),
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: false,
                    objects: Vec::new(),
                    npc_groups: Vec::new(),
                    triggers: Vec::new(),
                    spawned: false,
                    cleared: false,
                }],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: ctx.now_unix(),
            },
        );
    });
    let base_wcn = ctx
        .sde
        .types
        .get(&587)
        .and_then(|t| t.default_attributes.get(&153).copied())
        .unwrap_or(1.24e-6);
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_fc)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level.min(5))
            .unwrap_or(0)
    });
    let eff_wcn = base_wcn * (1.0 - 0.10 * (wdo_lvl as f64));
    let cap_for_160km = (eff_wcn * 1_000_000.0 / 149_597_870_700.0) * 160_000.0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            far_out_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: far_out_bm_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Far Outside".into(),
                x: 10_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    sys_handle.try_remove_ball(ship_fc);
    let mut low_cap_cs = eve_destiny::CombatState::default();
    low_cap_cs.max_capacitor = 500.0;
    low_cap_cs.capacitor = cap_for_160km;
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_fc,
                type_id: 587,
                position: entry_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            low_cap_cs,
        )
        .await
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_fc);
    let res_cap_clamp = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_BOOKMARK.into()),
            PyRep::Integer(far_out_bm_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_cap_clamp, PyRep::Bool(true));
    let notes = ctx.drain_notifications_for_char(char_fc);
    assert!(
        !notes.iter().any(|n| n.event == "OnExitingDungeon"),
        "capacitor-clamped warp landing inside same dungeon must not emit OnExitingDungeon"
    );
}

#[tokio::test]
async fn mission_pocket_wave_respawn_is_scoped_to_warped_mission_and_preserves_npcs_remaining() {
    const LOC_DUNGEON: &str = "dungeon";

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 951_400_001_i64;
    let mid_a = 1_881_u32;
    let mid_b = 1_882_u32;
    let kw = HashMap::new();

    let origin_a = deterministic_mission_site_origin(&ctx, sys_id, mid_a);
    let start_pos = origin_a + glam::DVec3::new(350_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.active_missions.insert(
            (char_id, mid_a),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: mid_a,
                agent_id: 3_012_050,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, mid_b),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: mid_b,
                agent_id: 3_012_051,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 3,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.sp6_lifecycle
            .missions_awaiting_downtime_respawn
            .push((char_id, mid_b));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: start_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);

    let res = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(LOC_DUNGEON.into()),
            PyRep::Integer(i64::from(mid_a)),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let m_a = &snap.active_missions[&(char_id, mid_a)];
        assert_eq!(
            m_a.npcs_remaining, 2,
            "warping to mission A must preserve cur_npcs (2) instead of inflating to .max(4)"
        );
        assert_eq!(m_a.spawned_npc_ball_ids.len(), 2);

        let m_b = &snap.active_missions[&(char_id, mid_b)];
        assert_eq!(m_b.npcs_remaining, 3);
        assert!(
            m_b.spawned_npc_ball_ids.is_empty(),
            "warping to mission A must NOT spawn mission B's wave at mission A's anchor_pos"
        );
        assert!(
            snap.sp6_lifecycle
                .missions_awaiting_downtime_respawn
                .contains(&(char_id, mid_b)),
            "mission B must remain awaiting downtime respawn until pilot warps to mission B"
        );
    });
}

#[tokio::test]
async fn agent_fleet_warp_zero_warped_skips_wave_and_partial_warped_runs_hooks_only_for_warped() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_fc = 90_000_201_i64;
    let char_wing = 90_000_202_i64;
    let ship_fc = 952_100_001_i64;
    let ship_wing = 952_100_002_i64;
    let fleet_id = 777_950_i64;
    let mission_id = 95_201_u32;
    let bm_id = 952_200_001_i64;
    let kw = HashMap::new();

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .expect("system must have SDE agent");
    let start_pos = glam::DVec3::new(1_000_000.0, 0.0, 0.0);
    let pocket_pos = glam::DVec3::new(10_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_fc);
        let template_ch = snap.characters.values().next().unwrap().clone();
        let template_acc = snap.accounts.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 952_001 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("fleet_warp_user_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| template_ch.clone());
            ch.character_id = cid;
            ch.user_id = uid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Pocket Objective".into(),
                x: pocket_pos.x,
                y: pocket_pos.y,
                z: pocket_pos.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_fc, mission_id),
            ActiveMissionRecord {
                character_id: char_fc,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(bm_id),
            },
        );
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            eve_db::FleetMemberRecord {
                character_id: char_wing,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        for (inst_id, owner) in [(958_201_i64, char_fc), (958_202_i64, char_wing)] {
            store.instances.insert(
                inst_id,
                eve_db::DungeonRuntimeInstance {
                    instance_id: inst_id,
                    dungeon_id: 9_821,
                    dungeon_name: "Fleet Pocket Exit".into(),
                    category: "Combat".into(),
                    archetype: "Deadspace".into(),
                    solar_system_id: sys_id,
                    owner_char_id: owner,
                    base_position: start_pos.to_array(),
                    disallow_mwd: false,
                    entry_gate_ball_id: None,
                    entry_gate_pos: None,
                    entry_gate_locked: false,
                    current_room_index: 0,
                    rooms: vec![eve_db::DungeonRuntimeRoom {
                        room_index: 0,
                        room_id: 9_821_00,
                        room_name: "Entry".into(),
                        position: start_pos.to_array(),
                        gate_destination_pos: start_pos.to_array(),
                        gated: false,
                        disallow_mwd: None,
                        gate_ball_id: None,
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: false,
                        cleared: false,
                    }],
                    completed: false,
                    despawned: false,
                    active_objective_id: 0,
                    last_completed_task_id: None,
                    completed_task_ids: Vec::new(),
                    created_unix: ctx.now_unix(),
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = eve_destiny::CombatState::default();
        cs.capacitor = 0.0;
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: start_pos + offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
    }

    let now = ctx.now_unix();
    for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
        ctx.db
            .acquire_online_lock(952_001 + idx as u64, 952_500 + idx as u64, Some(cid), true, now)
            .expect("online lock");
        crate::services::space_destiny::grant_location_change_invulnerability(&ctx, cid, now + 60);
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
            st.safe_logoffs.insert(
                cid,
                eve_db::SafeLogoffEntry {
                    char_id: cid,
                    ship_id: sid,
                    solar_system_id: sys_id,
                    started_unix: now,
                    complete_at_unix: now + 30,
                    completed: false,
                },
            );
        });
    }
    let _ = ctx.drain_notifications_for_char(char_fc);
    let _ = ctx.drain_notifications_for_char(char_wing);

    let mut sess = docked_session(char_fc, sys_id, None);
    sess.ship_id = Some(ship_fc);
    sess.fleet_id = Some(fleet_id);

    // 1. Both ships have 0 GJ cap -> 0 ships warp -> returns false, no wave spawned, hooks skipped.
    let res_zero = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_zero, PyRep::Bool(false));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.active_missions[&(char_fc, mission_id)].spawned_npc_ball_ids.is_empty());
        assert!(snap.sp6_lifecycle.safe_logoffs.contains_key(&char_fc));
        assert!(snap.sp6_lifecycle.safe_logoffs.contains_key(&char_wing));
    });
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_fc, ctx.now_unix()));
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_wing, ctx.now_unix()));

    // 2. Restore wingmate capacitor only -> FC skipped, wingmate warps -> wave spawns and ONLY
    //    warped wingmate runs `on_warp_initiated_hooks` & `on_warp_start_leave_dungeon_if_needed`.
    sys_handle.set_capacitor(ship_wing, 250.0).await.unwrap();
    let res_partial = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_partial, PyRep::Bool(true));

    let (_, fc_b) = sys_handle.get_ball_snapshot(ship_fc).await.unwrap().unwrap();
    let (_, wing_b) = sys_handle.get_ball_snapshot(ship_wing).await.unwrap().unwrap();
    assert!(matches!(fc_b.mode, eve_destiny::MovementMode::Stop));
    assert!(matches!(wing_b.mode, eve_destiny::MovementMode::Warp { .. }));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_fc, mission_id)].spawned_npc_ball_ids.len(), 2);
        assert!(snap.sp6_lifecycle.safe_logoffs.contains_key(&char_fc));
        assert!(!snap.sp6_lifecycle.safe_logoffs.contains_key(&char_wing));
    });
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_fc, ctx.now_unix()));
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_wing, ctx.now_unix()));

    let fc_notes = ctx.drain_notifications_for_char(char_fc);
    let wing_notes = ctx.drain_notifications_for_char(char_wing);
    assert!(!fc_notes.iter().any(|n| n.event == "OnExitingDungeon"));
    assert!(wing_notes.iter().any(|n| n.event == "OnExitingDungeon"));
}

#[tokio::test]
async fn courier_legacy_mission_id_warps_to_destination_station_and_explicit_secondary_fallback() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_301_i64;
    let ship_id = 953_100_001_i64;
    let mission_id = 96_301_u32;
    let auto_bm_id = 953_200_001_i64;
    let sec_bm_id = 953_200_002_i64;
    let kw = HashMap::new();

    let mut agents_in_sys: Vec<_> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.solar_system_id == Some(sys_id)
                && a.station_id
                    .is_some_and(|st_id| ctx.sde.stations.contains_key(&st_id))
        })
        .collect();
    agents_in_sys.sort_by_key(|a| a.agent_id);
    let agent = agents_in_sys
        .into_iter()
        .next()
        .expect("system must have SDE station agent");
    let origin_st = agent.station_id.unwrap();
    let mut stations_in_sys: Vec<_> = ctx
        .sde
        .stations
        .values()
        .filter(|st| st.solar_system_id == sys_id && st.station_id != origin_st)
        .collect();
    stations_in_sys.sort_by_key(|st| st.station_id);
    let dest_st = stations_in_sys
        .iter()
        .copied()
        .next()
        .expect("destination station in system");
    let alt_st = stations_in_sys
        .into_iter()
        .find(|st| {
            st.station_id != dest_st.station_id
                && st.position.distance(dest_st.position) > 500_000.0
        })
        .expect("alternate distinct station in system");

    let start_pos = glam::DVec3::new(2_000_000_000.0, 2_000_000_000.0, 0.0);
    let auto_bm_pos = start_pos + glam::DVec3::new(0.0, 800_000.0, 0.0);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        let template_ch = snap.characters.values().next().unwrap().clone();
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ch = snap.characters.get(&char_id).cloned().unwrap_or_else(|| template_ch);
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);

        snap.bookmarks.insert(
            auto_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: auto_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Auto Courier Pocket".into(),
                x: auto_bm_pos.x,
                y: auto_bm_pos.y,
                z: auto_bm_pos.z,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            sec_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: sec_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Secondary Dropoff".into(),
                x: alt_st.position.x,
                y: alt_st.position.y,
                z: alt_st.position.z,
                item_id: Some(alt_st.station_id),
                memo: "Mission Pocket Secondary".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_id, mission_id),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(auto_bm_id),
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id,
                agent_id: agent.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: Some(now),
                accept_expiry_unix: Some(now + 604_800),
                bonus_expiry_unix: Some(now + 3_600),
                completed_at_unix: None,
                required_delivery_type_id: Some(34),
                required_delivery_qty: 20,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: Some(dest_st.station_id),
                origin_station_id: Some(origin_st),
                offered_level: Some(1),
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: start_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);

    // 1. Legacy WarpToLocation("objective.destination", mission_id) must warp to
    //    meta.destination_station_id rather than auto-created non-station m.bookmark_id.
    let res_legacy = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("objective.destination".into()),
            PyRep::Integer(i64::from(mission_id)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_legacy, PyRep::Bool(true));
    let (_, b1) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b1.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(dest_st.position) < 100_000.0,
                "legacy courier mission_id must warp to dest_st ({:?}), got {destination:?}",
                dest_st.position
            );
        }
        other => panic!("expected Warp to dest_st, got {other:?}"),
    }

    // 2. Explicit ordinal 1 selecting a valid station bookmark short-circuits ahead of dest_sid.
    let _ = sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.max_capacitor = 10_000.0;
            cs.capacitor = 10_000.0;
        })
        .await
        .unwrap()
        .unwrap();
    let res_sec = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("objective.destination".into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_sec, PyRep::Bool(true));
    let (_, b2) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b2.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(alt_st.position) < 100_000.0,
                "explicit station bookmark must warp to alt_st ({:?}), got {destination:?}",
                alt_st.position
            );
        }
        other => panic!("expected Warp to alt_st, got {other:?}"),
    }

    // 3. Non-station secondary bookmark falls through cleanly to dest_st.
    let _ = sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.max_capacitor = 10_000.0;
            cs.capacitor = 10_000.0;
        })
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.get_mut(&sec_bm_id).unwrap().item_id = None;
    });
    let res_non_station = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("objective.destination".into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_non_station, PyRep::Bool(true));
    let (_, b3) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b3.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(dest_st.position) < 100_000.0);
        }
        other => panic!("expected fallback Warp to dest_st, got {other:?}"),
    }

    // 4. Expired secondary station bookmark also falls through cleanly to dest_st.
    let _ = sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.max_capacitor = 10_000.0;
            cs.capacitor = 10_000.0;
        })
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.get_mut(&sec_bm_id).unwrap().item_id = Some(alt_st.station_id);
        snap.bookmark_expiries.insert(sec_bm_id, now.saturating_sub(1));
    });
    let res_expired = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("objective.destination".into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_expired, PyRep::Bool(true));
    let (_, b4) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b4.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(dest_st.position) < 100_000.0);
        }
        other => panic!("expected expired fallback Warp to dest_st, got {other:?}"),
    }
}

#[tokio::test]
async fn find_accepted_mission_ordinal_precedes_small_mission_id_under_agent_selector() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_401_i64;
    let ship_id = 954_100_001_i64;
    let mission_id = 1_u32;
    let bm0_id = 954_200_001_i64;
    let bm1_id = 954_200_002_i64;
    let kw = HashMap::new();

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .expect("system must have SDE agent");
    let start_pos = glam::DVec3::new(0.0, 0.0, 0.0);
    let bm0_pos = glam::DVec3::new(0.0, 600_000.0, 0.0);
    let bm1_pos = glam::DVec3::new(0.0, 0.0, 900_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        let template_ch = snap.characters.values().next().unwrap().clone();
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ch = snap.characters.get(&char_id).cloned().unwrap_or_else(|| template_ch);
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);

        for (bmid, pos, label) in [
            (bm0_id, bm0_pos, "Pocket 0"),
            (bm1_id, bm1_pos, "Pocket 1"),
        ] {
            snap.bookmarks.insert(
                bmid,
                eve_db::BookmarkRecord {
                    bookmark_id: bmid,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    label: label.into(),
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    item_id: None,
                    memo: "Mission Pocket".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
        snap.active_missions.insert(
            (char_id, mission_id),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(bm0_id),
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: start_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    let bound = bound_agent(char_id, agent.agent_id);

    // Both unbound referringAgentID and bound agent moniker with mission_id == 1 (< 64) and
    // loc == 1 must resolve ordinal bookmark #1 (bm1_pos), not m.bookmark_id (bm0_pos).
    let res_ref = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_ref, PyRep::Bool(true));
    let (_, b_ref) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b_ref.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(bm1_pos) < 1.0);
        }
        other => panic!("expected Warp to bm1, got {other:?}"),
    }

    let _ = sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    let res_bound = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(1),
            PyRep::Real(0.0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_bound, PyRep::Bool(true));
    let (_, b_bound) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b_bound.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(bm1_pos) < 1.0);
        }
        other => panic!("expected Warp to bm1 via bound agent, got {other:?}"),
    }
}

#[tokio::test]
async fn agent_fleet_warp_deduplicates_invuln_cancel_and_enforces_destination_bubble_interception() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let char_fc = 90_000_981_i64;
    let char_wing = 90_000_982_i64;
    let ship_fc = 959_100_001_i64;
    let ship_wing = 959_100_002_i64;
    let fleet_id = 959_001_i64;
    let mission_id = 95_901_u32;
    let bm_id = 959_200_001_i64;

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .expect("system must have SDE agent");
    let start_pos = glam::DVec3::new(1_000_000.0, 0.0, 0.0);
    let pocket_pos = glam::DVec3::new(20_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_fc);
        let template_ch = snap.characters.values().next().unwrap().clone();
        let template_acc = snap.accounts.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 959_001 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("agent_fleet_parity_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Pocket Objective".into(),
                x: pocket_pos.x,
                y: pocket_pos.y,
                z: pocket_pos.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_fc, mission_id),
            ActiveMissionRecord {
                character_id: char_fc,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(bm_id),
            },
        );
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            eve_db::FleetMemberRecord {
                character_id: char_wing,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        for (inst_id, owner) in [(959_301_i64, char_fc), (959_302_i64, char_wing)] {
            store.instances.insert(
                inst_id,
                eve_db::DungeonRuntimeInstance {
                    instance_id: inst_id,
                    dungeon_id: 9_921,
                    dungeon_name: "Agent Fleet Parity Pocket".into(),
                    category: "Deadspace".into(),
                    archetype: "Deadspace".into(),
                    solar_system_id: sys_id,
                    owner_char_id: owner,
                    base_position: start_pos.to_array(),
                    disallow_mwd: false,
                    entry_gate_ball_id: None,
                    entry_gate_pos: None,
                    entry_gate_locked: false,
                    current_room_index: 0,
                    rooms: vec![eve_db::DungeonRuntimeRoom {
                        room_index: 0,
                        room_id: 9_921_00,
                        room_name: "Entry".into(),
                        position: start_pos.to_array(),
                        gate_destination_pos: start_pos.to_array(),
                        gated: false,
                        disallow_mwd: None,
                        gate_ball_id: None,
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: false,
                        cleared: false,
                    }],
                    completed: false,
                    despawned: false,
                    active_objective_id: 0,
                    last_completed_task_id: None,
                    completed_task_ids: Vec::new(),
                    created_unix: ctx.now_unix(),
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = eve_destiny::CombatState::default();
        cs.undock_invulnerable_remaining_ticks = 20;
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: start_pos + offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
    }
    sys_handle
        .add_warp_bubble(959_401, pocket_pos - glam::DVec3::new(100_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();

    let now = ctx.now_unix();
    for (idx, cid) in [char_fc, char_wing].into_iter().enumerate() {
        ctx.db
            .acquire_online_lock(959_001 + idx as u64, 959_500 + idx as u64, Some(cid), true, now)
            .expect("online lock");
        crate::services::space_destiny::grant_location_change_invulnerability(&ctx, cid, now + 60);
    }
    let _ = ctx.drain_notifications_for_char(char_fc);
    let _ = ctx.drain_notifications_for_char(char_wing);

    let mut sess = docked_session(char_fc, sys_id, None);
    sess.user_id = Some(959_001);
    sess.session_id = 959_500;
    sess.ship_id = Some(ship_fc);
    sess.fleet_id = Some(fleet_id);

    let res = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res, PyRep::Bool(true));

    // Destination catch bubble intercepted both fleetmates before pocket_pos.
    for sid in [ship_fc, ship_wing] {
        let (_, b) = sys_handle.get_ball_snapshot(sid).await.unwrap().unwrap();
        match b.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert!(
                    destination.x < pocket_pos.x - 50_000.0,
                    "ship {sid} must be caught by destination warp bubble, got {destination:?}"
                );
            }
            other => panic!("expected Warp mode for ship {sid}, got {other:?}"),
        }
    }

    // Both OnInvulnCancelled and OnExitingDungeon fire strictly once per warped member.
    for cid in [char_fc, char_wing] {
        let notes = ctx.drain_notifications_for_char(cid);
        assert_eq!(
            notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(),
            1,
            "char {cid} must receive exactly one OnInvulnCancelled"
        );
        assert_eq!(
            notes.iter().filter(|n| n.event == "OnExitingDungeon").count(),
            1,
            "char {cid} must receive exactly one OnExitingDungeon"
        );
    }
}
