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
